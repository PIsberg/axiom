use axiom_core::mcp::{AxiomMcpServer, JsonRpcRequest};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempRepo {
    root: PathBuf,
}

impl TempRepo {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("axiom-reg-{}-{}-{}", tag, std::process::id(), n));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create repo dir");
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn write(&self, relative_path: &str, content: &str) {
        let full = self.root.join(relative_path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, content).expect("write file");
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn test_full_regression_indexing_and_agent_workflow() {
    let repo = TempRepo::new("acme_e2e");

    // 1. Setup multi-tier repository on disk with DI, Interfaces, and Tests
    let repo_interface = r#"
package com.acme.order;

public interface OrderRepository {
    void saveOrder(String orderId, double amount);
}
"#;

    let repo_impl = r#"
package com.acme.order;
import org.springframework.stereotype.Repository;

@Repository
public class JpaOrderRepository implements OrderRepository {
    public void saveOrder(String orderId, double amount) {}
}
"#;

    // Service using Spring constructor injection for managed component
    let service_code = r#"
package com.acme.order;
import org.springframework.stereotype.Service;

@Service
public class OrderService {
    private final OrderRepository orderRepository;

    public OrderService(OrderRepository orderRepository) {
        this.orderRepository = orderRepository;
    }

    public void createOrder(String orderId, double amount) {
        orderRepository.saveOrder(orderId, amount);
    }
}
"#;

    let test_code = r#"
package com.acme.order;
import org.junit.Test;

public class OrderServiceTest {
    @Test
    public void testCreateOrder() {
        OrderRepository repo = new JpaOrderRepository();
        OrderService svc = new OrderService(repo);
        svc.createOrder("ord-123", 42.0);
    }
}
"#;

    repo.write("src/main/java/com/acme/order/OrderRepository.java", repo_interface);
    repo.write("src/main/java/com/acme/order/JpaOrderRepository.java", repo_impl);
    repo.write("src/main/java/com/acme/order/OrderService.java", service_code);
    repo.write("src/test/java/com/acme/order/OrderServiceTest.java", test_code);

    // 2. Initial Indexing and disk persistence
    let axiom_dir = repo.path().join(".axiom");
    std::fs::create_dir_all(&axiom_dir).unwrap();
    let index_file = axiom_dir.join("index.json");

    let index = axiom_ast::AstIndex::new();
    index.scan_directory(repo.path()).expect("scan directory");
    index.save_to_disk(&index_file).expect("save index to disk");

    // Verify DI bindings and blast radius in the AST index
    let di_consumers = index.get_di_consumers("OrderRepository");
    assert!(
        di_consumers.iter().any(|c| c.contains("OrderService")),
        "OrderService should be registered as DI consumer of OrderRepository, got: {:?}",
        di_consumers
    );

    let br = index.compute_blast_radius("com.acme.order.JpaOrderRepository", 3)
        .expect("blast radius for JpaOrderRepository");
    assert!(
        br.impacted_tests.iter().any(|t| t.contains("OrderServiceTest")),
        "Blast radius of JpaOrderRepository must reach OrderServiceTest through interface & DI, got: {:?}",
        br.impacted_tests
    );

    // 3. Boot MCP server against the persisted repo index
    let server = AxiomMcpServer::with_index(Some(&index_file)).expect("server starts");

    // Initialize check
    let init_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "initialize".to_string(),
        params: None,
    };
    let init_res = server.handle_request(init_req).await;
    assert!(init_res.error.is_none());

    // 4. Test Dynamic Sub-Graph Context Prompts
    // axiom_review_patch prompt expands with pre-computed slice, impacted tests, and causal paths
    let prompt_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "prompts/get".to_string(),
        params: Some(json!({
            "name": "axiom_review_patch",
            "arguments": {
                "symbol_path": "JpaOrderRepository"
            }
        })),
    };
    let prompt_res = server.handle_request(prompt_req).await;
    assert!(prompt_res.error.is_none());
    let prompt_text = prompt_res.result.unwrap()["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(prompt_text.contains("com.acme.order.JpaOrderRepository"));
    assert!(prompt_text.contains("OrderServiceTest"));
    assert!(prompt_text.contains("Pre-Computed Sub-Graph Context"));

    // 5. Test Verified Fix Cache & Patch Memory Lifecycle
    // Record execution verification
    let record_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "axiom_record_verification",
            "arguments": {
                "task_id": "ctop_task_404",
                "command": "mvn test -Dtest=OrderServiceTest",
                "passed": true
            }
        })),
    };
    let record_res = server.handle_request(record_req).await;
    assert!(record_res.error.is_none());

    // Attest commit with error_signature and patch_content
    let error_sig = "OrderValidationException: amount must be positive";
    let patch_body = "if (amount <= 0.0) throw new IllegalArgumentException(\"Invalid amount\");";
    let attest_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(4)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "axiom_attest_commit",
            "arguments": {
                "prompt": "Validate non-negative order amounts",
                "symbol_path": "com.acme.order.OrderService::createOrder",
                "ctop_task_id": "ctop_task_404",
                "error_signature": error_sig,
                "patch_content": patch_body
            }
        })),
    };
    let attest_res = server.handle_request(attest_req).await;
    assert!(attest_res.error.is_none());
    let attest_content = attest_res.result.unwrap();
    assert_eq!(attest_content["isError"], false);

    // Query axiom://fixes resource
    let fixes_res = server.handle_request(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(5)),
        method: "resources/read".to_string(),
        params: Some(json!({ "uri": "axiom://fixes" })),
    }).await;
    assert!(fixes_res.error.is_none());
    let fixes_json: serde_json::Value = serde_json::from_str(
        fixes_res.result.unwrap()["contents"][0]["text"].as_str().unwrap()
    ).unwrap();
    assert_eq!(fixes_json["count"], 1);

    // 0ms patch memory lookup matching error signature
    let matching_fixes = server.find_matching_fixes("any_hash", error_sig);
    assert_eq!(matching_fixes.len(), 1);
    assert_eq!(matching_fixes[0].patch_content, patch_body);

    // 6. Test Incremental Rescan / Editing Code in the Repository
    // Add another injected dependency into OrderService
    let updated_service = r#"
package com.acme.order;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.stereotype.Service;

@Service
public class OrderService {
    @Autowired
    private OrderRepository orderRepository;

    @Autowired
    private PaymentGateway paymentGateway;

    public void createOrder(String orderId, double amount) {
        paymentGateway.processPayment(amount);
        orderRepository.saveOrder(orderId, amount);
    }
}
"#;
    let payment_interface = r#"
package com.acme.order;

public interface PaymentGateway {
    void processPayment(double amount);
}
"#;
    repo.write("src/main/java/com/acme/order/OrderService.java", updated_service);
    repo.write("src/main/java/com/acme/order/PaymentGateway.java", payment_interface);

    // Incremental scan
    let reloaded_index = axiom_ast::AstIndex::load_from_disk(&index_file).expect("load disk index");
    reloaded_index.scan_directory(repo.path()).expect("rescan repo");
    reloaded_index.save_to_disk(&index_file).expect("resave disk index");

    // Both OrderRepository and PaymentGateway should now be injected
    let order_consumers = reloaded_index.get_di_consumers("OrderRepository");
    let payment_consumers = reloaded_index.get_di_consumers("PaymentGateway");
    assert!(order_consumers.iter().any(|c| c.contains("OrderService")));
    assert!(payment_consumers.iter().any(|c| c.contains("OrderService")));

    // 7. Restart Server from persistent directory and ensure complete fidelity
    drop(server);
    let restarted = AxiomMcpServer::with_index(Some(&index_file)).expect("reloaded server");

    let restarted_fixes = restarted.handle_request(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(6)),
        method: "resources/read".to_string(),
        params: Some(json!({ "uri": "axiom://fixes" })),
    }).await;
    assert!(restarted_fixes.error.is_none());
    let restarted_json: serde_json::Value = serde_json::from_str(
        restarted_fixes.result.unwrap()["contents"][0]["text"].as_str().unwrap()
    ).unwrap();
    assert_eq!(restarted_json["count"], 1);

    // Blast radius across reloaded server
    let reloaded_br = restarted.ast_index.compute_blast_radius("com.acme.order.PaymentGateway", 3)
        .expect("blast radius for PaymentGateway");
    assert!(reloaded_br.impacted_tests.iter().any(|t| t.contains("OrderServiceTest")));
}
