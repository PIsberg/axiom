use axiom_ast::AstIndex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("axiom-hier-{}-{}-{}", tag, std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the test directory");
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

    fn remove(&self, name: &str) {
        let file = self.0.join(name);
        let _ = std::fs::remove_file(file);
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scan(dir: &TempDir) -> AstIndex {
    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan fixture");
    index
}

#[test]
fn test_java_interface_and_inheritance_blast_radius() {
    let dir = TempDir::new("java_hier");

    let repo_interface = r#"
package com.example.repo;

public interface UserRepository {
    User findById(String id);
}
"#;

    let repo_impl = r#"
package com.example.repo;

public class SqlUserRepository implements UserRepository {
    public User findById(String id) {
        return new User(id);
    }
}
"#;

    let repo_test = r#"
package com.example.repo;

public class SqlUserRepositoryTest {
    @Test
    public void testFindById() {
        SqlUserRepository repo = new SqlUserRepository();
        assertNotNull(repo.findById("u1"));
    }
}
"#;

    dir.write(
        "src/main/java/com/example/repo/UserRepository.java",
        repo_interface,
    );
    dir.write(
        "src/main/java/com/example/repo/SqlUserRepository.java",
        repo_impl,
    );
    dir.write(
        "src/test/java/com/example/repo/SqlUserRepositoryTest.java",
        repo_test,
    );

    let index = scan(&dir);

    // Verify inheritance was registered
    let impls = index.get_implementors("UserRepository");
    assert!(
        impls.contains(&"SqlUserRepository".to_string())
            || impls.contains(&"com.example.repo.SqlUserRepository".to_string())
    );

    let supertypes = index.get_supertypes("SqlUserRepository");
    assert!(supertypes.contains(&"UserRepository".to_string()));

    // Verify blast radius of interface includes the test for the implementing class
    let radius = index.compute_blast_radius("com.example.repo.UserRepository", 3);
    assert!(radius.is_some(), "blast radius should not be None");
    let r = radius.unwrap();
    assert!(
        r.impacted_tests
            .iter()
            .any(|t| t.contains("SqlUserRepositoryTest")),
        "Blast radius of UserRepository must include SqlUserRepositoryTest, got: {:?}",
        r.impacted_tests
    );
}

#[test]
fn test_typescript_interface_blast_radius() {
    let dir = TempDir::new("ts_hier");

    let iface = r#"
export interface IAuthService {
    authenticate(token: string): boolean;
}
"#;

    let impl_ts = r#"
import { IAuthService } from "./auth";

export class JwtAuthService implements IAuthService {
    authenticate(token: string): boolean {
        return token.length > 0;
    }
}
"#;

    let test_ts = r#"
import { JwtAuthService } from "./jwt";

export function testJwtAuth() {
    const auth = new JwtAuthService();
    if (!auth.authenticate("valid")) throw new Error("failed");
}
"#;

    dir.write("src/auth.ts", iface);
    dir.write("src/jwt.ts", impl_ts);
    dir.write("src/jwt.test.ts", test_ts);

    let index = scan(&dir);

    let impls = index.get_implementors("IAuthService");
    assert!(
        impls.contains(&"JwtAuthService".to_string())
            || impls.iter().any(|s| s.contains("JwtAuthService"))
    );

    let sym = index
        .symbol_paths()
        .into_iter()
        .find(|s| s.contains("IAuthService"))
        .expect("IAuthService symbol not found");
    let radius = index.compute_blast_radius(&sym, 3);
    assert!(radius.is_some());
    let r = radius.unwrap();
    assert!(
        r.impacted_tests
            .iter()
            .any(|t| t.contains("testJwtAuth") || t.contains("jwt.test")),
        "Blast radius of IAuthService must include jwt test, got: {:?}",
        r.impacted_tests
    );
}

#[test]
fn test_rust_trait_impl_blast_radius() {
    let dir = TempDir::new("rust_hier");

    let trait_rs = r#"
pub trait Engine {
    fn start(&self);
}
"#;

    let impl_rs = r#"
use crate::engine::Engine;

pub struct V8Engine;

impl Engine for V8Engine {
    fn start(&self) {}
}
"#;

    let test_rs = r#"
use crate::v8::V8Engine;

#[test]
fn test_v8_start() {
    let engine = V8Engine;
    engine.start();
}
"#;

    dir.write("src/engine.rs", trait_rs);
    dir.write("src/v8.rs", impl_rs);
    dir.write("tests/v8_test.rs", test_rs);

    let index = scan(&dir);

    let impls = index.get_implementors("Engine");
    assert!(impls.contains(&"V8Engine".to_string()));

    let sym = index
        .symbol_paths()
        .into_iter()
        .find(|s| s.contains("Engine") && !s.contains("V8"))
        .expect("Engine trait symbol not found");
    let radius = index.compute_blast_radius(&sym, 3);
    assert!(radius.is_some());
    let r = radius.unwrap();
    assert!(
        r.impacted_tests
            .iter()
            .any(|t| t.contains("test_v8_start") || t.contains("v8_test")),
        "Blast radius of Engine trait must include v8 test, got: {:?}",
        r.impacted_tests
    );
}

#[test]
fn test_cpp_inheritance_blast_radius() {
    let dir = TempDir::new("cpp_hier");

    let base_cpp = r#"
class BaseDevice {
public:
    virtual void reset() = 0;
};
"#;

    let derived_cpp = r#"
#include "base.h"

class UsbDevice : public BaseDevice {
public:
    void reset() override {}
};
"#;

    let test_cpp = r#"
#include "usb.h"

void test_usb_reset() {
    UsbDevice dev;
    dev.reset();
}
"#;

    dir.write("include/base.h", base_cpp);
    dir.write("src/usb.cpp", derived_cpp);
    dir.write("tests/test_usb.cpp", test_cpp);

    let index = scan(&dir);

    let impls = index.get_implementors("BaseDevice");
    assert!(
        impls.contains(&"UsbDevice".to_string()) || impls.iter().any(|s| s.contains("UsbDevice"))
    );

    let sym = index
        .symbol_paths()
        .into_iter()
        .find(|s| s.contains("BaseDevice"))
        .expect("BaseDevice symbol not found");
    let radius = index.compute_blast_radius(&sym, 3);
    assert!(radius.is_some());
    let r = radius.unwrap();
    assert!(
        r.impacted_tests.iter().any(|t| t.contains("test_usb")),
        "Blast radius of BaseDevice must include test_usb, got: {:?}",
        r.impacted_tests
    );
}

#[test]
fn test_persistence_preserves_hierarchy() {
    let dir = TempDir::new("persist_hier");
    let index_file = dir.path().join("index.json");

    let index = AstIndex::new();
    index.register_inheritance("Dog", "Animal");
    index.register_inheritance("Cat", "Animal");

    index
        .save_to_disk(&index_file)
        .expect("save should succeed");

    let loaded = AstIndex::load_from_disk(&index_file).expect("load should succeed");
    let mut impls = loaded.get_implementors("Animal");
    impls.sort();
    assert_eq!(impls, vec!["Cat".to_string(), "Dog".to_string()]);

    let supertypes = loaded.get_supertypes("Dog");
    assert_eq!(supertypes, vec!["Animal".to_string()]);
}

#[test]
fn test_qualified_and_generic_hierarchy_across_languages() {
    let dir = TempDir::new("qual_generic_hier");

    // Rust reference trait impl
    let rust_code = r#"
pub trait StreamHandler {
    fn handle(&self);
}

pub struct TcpStream;

impl<'a> StreamHandler for &'a TcpStream {
    fn handle(&self) {}
}

#[test]
fn test_tcp_stream() {
    let s = TcpStream;
    (&s).handle();
}
"#;

    // Python generic inheritance
    let py_code = r#"
class BaseEntity:
    pass

class UserModel(Generic[T], BaseEntity):
    pass

def test_user_model():
    m = UserModel()
    assert m is not None
"#;

    // Java qualified implementation
    let java_code = r#"
package com.app;

public class CustomService implements org.framework.api.IService {
    public void execute() {}
}

public class CustomServiceTest {
    @Test
    public void testExec() {
        CustomService s = new CustomService();
        s.execute();
    }
}
"#;

    dir.write("src/stream.rs", rust_code);
    dir.write("src/model.py", py_code);
    dir.write("src/CustomService.java", java_code);

    let index = scan(&dir);

    // Verify Rust reference trait impl
    let rust_impls = index.get_implementors("StreamHandler");
    assert!(
        rust_impls.contains(&"TcpStream".to_string()),
        "Rust reference trait impl should register TcpStream for StreamHandler, got: {:?}",
        rust_impls
    );

    // Verify Python generic base inheritance
    let py_impls = index.get_implementors("BaseEntity");
    assert!(
        py_impls.contains(&"UserModel".to_string())
            || py_impls.iter().any(|s| s.contains("UserModel")),
        "Python generic inheritance should register UserModel for BaseEntity, got: {:?}",
        py_impls
    );

    // Verify Java qualified interface implementation
    let java_impls = index.get_implementors("IService");
    assert!(
        java_impls.contains(&"CustomService".to_string())
            || java_impls.iter().any(|s| s.contains("CustomService")),
        "Java qualified interface should register CustomService for IService, got: {:?}",
        java_impls
    );
}

#[test]
fn test_rescan_purges_deleted_type_hierarchy() {
    let dir = TempDir::new("rescan_purge_hier");

    let java_service = r#"
package com.app;

public class SqlUserRepo implements UserRepository {
    public void query() {}
}
"#;
    dir.write("src/SqlUserRepo.java", java_service);

    let index = scan(&dir);
    let impls = index.get_implementors("UserRepository");
    assert!(
        !impls.is_empty(),
        "Implementors should contain SqlUserRepo before deletion"
    );

    // Delete file and rescan
    dir.remove("src/SqlUserRepo.java");
    let rescan = scan(&dir);
    let impls_after = rescan.get_implementors("UserRepository");
    assert!(
        impls_after.is_empty(),
        "Implementors must be empty after deleting SqlUserRepo, got: {:?}",
        impls_after
    );
}
