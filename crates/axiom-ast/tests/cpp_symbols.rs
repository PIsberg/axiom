//! Comprehensive verification of C and C++ symbol parsing and indexing.

use axiom_ast::AstIndex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("axiom-cpp-{}-{}-{}", tag, std::process::id(), n));
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
        std::fs::write(file, body).expect("write the fixture file");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scan(dir: &TempDir) -> AstIndex {
    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan the fixture");
    index
}

#[test]
fn a_cpp_class_and_methods_are_indexed_under_namespace() {
    let dir = TempDir::new("cpp_class");
    dir.write(
        "engine.cpp",
        "namespace axiom::core {\n\
         \n\
         class Engine {\n\
         public:\n\
             Engine() {}\n\
             bool start(int timeout) {\n\
                 return timeout > 0;\n\
             }\n\
             void stop() {\n\
             }\n\
         };\n\
         \n\
         void global_init() {\n\
         }\n\
         }\n",
    );

    let index = scan(&dir);
    let symbols = index.symbol_paths();

    assert_eq!(
        index.candidates_for("Engine").len(),
        2,
        "Engine class and constructor both indexed: {symbols:?}"
    );
    assert!(
        index.get_symbol("core::Engine").is_some(),
        "Qualified Engine class lookup works: {symbols:?}"
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.ends_with("::axiom::core::Engine::start")),
        "Engine::start method indexed: {symbols:?}"
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.ends_with("::axiom::core::Engine::stop")),
        "Engine::stop method indexed: {symbols:?}"
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.ends_with("::axiom::core::global_init")),
        "global_init function indexed: {symbols:?}"
    );
}

#[test]
fn a_c_header_and_source_with_structs_and_functions_are_indexed() {
    let dir = TempDir::new("c_structs");
    dir.write(
        "include/buffer.h",
        "struct Buffer {\n\
             char* data;\n\
             size_t capacity;\n\
         };\n\
         \n\
         enum BufferState {\n\
             BUFFER_EMPTY,\n\
             BUFFER_READY\n\
         };\n\
         \n\
         struct Buffer* buffer_create(size_t cap);\n\
         void buffer_free(struct Buffer* b);\n",
    );
    dir.write(
        "src/buffer.c",
        "#include \"buffer.h\"\n\
         \n\
         struct Buffer* buffer_create(size_t cap) {\n\
             struct Buffer* b = malloc(sizeof(struct Buffer));\n\
             b->data = malloc(cap);\n\
             b->capacity = cap;\n\
             return b;\n\
         }\n\
         \n\
         void buffer_free(struct Buffer* b) {\n\
             if (b) {\n\
                 free(b->data);\n\
                 free(b);\n\
             }\n\
         }\n",
    );

    let index = scan(&dir);
    let symbols = index.symbol_paths();

    assert!(
        index.get_symbol("Buffer").is_some(),
        "struct Buffer indexed: {symbols:?}"
    );
    assert!(
        index.get_symbol("BufferState").is_some(),
        "enum BufferState indexed: {symbols:?}"
    );
    assert!(
        symbols.iter().any(|s| s.ends_with("::buffer_create")),
        "buffer_create function indexed: {symbols:?}"
    );
    assert!(
        symbols.iter().any(|s| s.ends_with("::buffer_free")),
        "buffer_free function indexed: {symbols:?}"
    );
}

#[test]
fn cpp_templates_and_operator_overloads_are_indexed() {
    let dir = TempDir::new("cpp_templates");
    dir.write(
        "vector.hpp",
        "namespace math {\n\
         \n\
         template <typename T>\n\
         struct Vector3 {\n\
             T x, y, z;\n\
             \n\
             Vector3<T> operator+(const Vector3<T>& other) const {\n\
                 return {x + other.x, y + other.y, z + other.z};\n\
             }\n\
             \n\
             T dot(const Vector3<T>& other) const {\n\
                 return x * other.x + y * other.y + z * other.z;\n\
             }\n\
         };\n\
         \n\
         template <typename T>\n\
         T magnitude(const Vector3<T>& v) {\n\
             return sqrt(v.dot(v));\n\
         }\n\
         }\n",
    );

    let index = scan(&dir);
    let symbols = index.symbol_paths();

    assert!(
        index.get_symbol("Vector3").is_some(),
        "template struct Vector3 indexed: {symbols:?}"
    );
    assert!(
        symbols.iter().any(|s| s.ends_with("::math::Vector3::dot")),
        "Vector3::dot method indexed: {symbols:?}"
    );
    assert!(
        symbols.iter().any(|s| s.ends_with("::math::magnitude")),
        "template function magnitude indexed: {symbols:?}"
    );
}
