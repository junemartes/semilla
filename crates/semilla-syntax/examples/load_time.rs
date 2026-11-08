//! Measures the time to load a workspace: `cargo run --release -p semilla-syntax --example load_time -- <dir>`.

fn main() {
    let dir = std::env::args().nth(1).expect("a workspace folder");
    let mut parse = std::time::Duration::ZERO;
    let mut files = 0;
    for entry in walk(std::path::Path::new(&dir)) {
        let text = std::fs::read_to_string(&entry).unwrap();
        let started = std::time::Instant::now();
        let _ = syn::parse_file(&text);
        parse += started.elapsed();
        files += 1;
    }
    let started = std::time::Instant::now();
    let workspace = semilla_syntax::Workspace::load(std::path::Path::new(&dir)).unwrap();
    println!("parse only: {:.2} s for {files} files", parse.as_secs_f64());
    println!("full load:  {:.2} s for {} items", started.elapsed().as_secs_f64(), workspace.items.len());
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if path.is_dir() && name != "target" && !name.starts_with('.') {
            out.extend(walk(&path));
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
    out
}
