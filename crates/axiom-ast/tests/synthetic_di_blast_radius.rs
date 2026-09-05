use axiom_ast::AstIndex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("axiom-di-{}-{}-{}", tag, std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test directory");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, name: &str, body: &str) {
        let file = self.0.join(name);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(file, body).expect("write fixture");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn test_synthetic_di_annotation_parsing_and_blast_radius() {
    let dir = TempDir::new("spring_di");

    let payment_interface = r#"
package com.example.service;

public interface PaymentGateway {
    void processPayment(double amount);
}
"#;

    let stripe_impl = r#"
package com.example.service;
import org.springframework.stereotype.Service;

@Service
public class StripeGateway implements PaymentGateway {
    public void processPayment(double amount) {}
}
"#;

    let order_service = r#"
package com.example.service;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.stereotype.Component;

@Component
public class OrderService {
    @Autowired
    private PaymentGateway paymentGateway;

    public void checkout(double amount) {
        paymentGateway.processPayment(amount);
    }
}
"#;

    let order_test = r#"
package com.example.service;
import org.junit.Test;

public class OrderServiceTest {
    @Test
    public void testCheckout() {
        OrderService svc = new OrderService();
        svc.checkout(100.0);
    }
}
"#;

    dir.write("src/main/java/com/example/service/PaymentGateway.java", payment_interface);
    dir.write("src/main/java/com/example/service/StripeGateway.java", stripe_impl);
    dir.write("src/main/java/com/example/service/OrderService.java", order_service);
    dir.write("src/test/java/com/example/service/OrderServiceTest.java", order_test);

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan directory");

    // Verify DI bindings detected
    let consumers = index.get_di_consumers("PaymentGateway");
    assert!(
        consumers.iter().any(|c| c.contains("OrderService")),
        "OrderService should be registered as DI consumer of PaymentGateway, got: {:?}",
        consumers
    );

    // Blast radius of PaymentGateway interface should reach OrderService and its test
    let br_gateway = index.compute_blast_radius("com.example.service.PaymentGateway", 3)
        .expect("blast radius for PaymentGateway");
    assert!(
        br_gateway.impacted_tests.iter().any(|t| t.contains("OrderServiceTest")),
        "Blast radius of PaymentGateway must impact OrderServiceTest, got: {:?}",
        br_gateway.impacted_tests
    );
    assert!(
        br_gateway.causal_paths.iter().any(|(k, _)| k.contains("OrderServiceTest")),
        "Causal path should exist for OrderServiceTest"
    );

    // Blast radius of StripeGateway should also propagate to OrderServiceTest via interface and DI
    let br_stripe = index.compute_blast_radius("com.example.service.StripeGateway", 3)
        .expect("blast radius for StripeGateway");
    assert!(
        br_stripe.impacted_tests.iter().any(|t| t.contains("OrderServiceTest")),
        "Blast radius of StripeGateway must impact OrderServiceTest via DI, got: {:?}",
        br_stripe.impacted_tests
    );

    // Test persistence of DI bindings
    let save_path = dir.path().join("persisted_index.json");
    index.save_to_disk(&save_path).expect("save index");

    let loaded = AstIndex::load_from_disk(&save_path).expect("load index");
    let loaded_consumers = loaded.get_di_consumers("PaymentGateway");
    assert!(
        loaded_consumers.iter().any(|c| c.contains("OrderService")),
        "Loaded index must retain DI consumers"
    );
}

#[test]
fn test_manual_di_binding_and_blast_radius() {
    let index = AstIndex::new();

    // Provider: PaymentService
    index.index_node("app::payment::PaymentService", "struct", "struct PaymentService;", vec![]);
    // Consumer: OrderManager (uses DI for PaymentService)
    index.index_node("app::order::OrderManager", "struct", "struct OrderManager;", vec![]);
    // Test: OrderTest calling OrderManager
    index.index_node(
        "tests::order_test",
        "test",
        "#[test] fn order_test() { OrderManager::new(); }",
        vec!["app::order::OrderManager".to_string()],
    );

    // Register synthetic DI binding
    index.register_di_binding("app::order::OrderManager", "app::payment::PaymentService");

    assert!(index.get_di_consumers("app::payment::PaymentService").contains(&"app::order::OrderManager".to_string()));
    assert!(index.get_di_providers("app::order::OrderManager").contains(&"app::payment::PaymentService".to_string()));

    let br = index.compute_blast_radius("app::payment::PaymentService", 3).expect("blast radius");
    assert!(br.impacted_tests.contains(&"tests::order_test".to_string()));
    assert!(br.causal_paths.contains_key("tests::order_test"));
    let path = &br.causal_paths["tests::order_test"];
    assert_eq!(
        path,
        &vec![
            "app::payment::PaymentService".to_string(),
            "app::order::OrderManager".to_string(),
            "tests::order_test".to_string(),
        ]
    );
}
