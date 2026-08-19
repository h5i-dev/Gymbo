//! Reports the coordinates found in plugin `<configuration>` across a tree of
//! POMs, for checking the scanner against real input rather than fixtures.
use std::path::Path;

fn main() {
    let root = std::env::args().nth(1).expect("a directory of POMs");
    let mut poms = 0usize;
    let mut found = 0usize;
    let mut with = 0usize;
    let mut sample = Vec::new();
    visit(Path::new(&root), &mut |path: &Path| {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(parsed) = jv_model::parse_pom(&text) else {
            return;
        };
        poms += 1;
        let mut here = 0usize;
        let build = parsed.model.build.iter();
        let profiles = parsed
            .model
            .profiles
            .iter()
            .filter_map(|p| p.build.as_ref());
        for build in build.chain(profiles) {
            for plugin in build.plugins.iter().chain(&build.plugin_management) {
                for dependency in &plugin.configuration_artifacts {
                    here += 1;
                    if true {
                        sample.push(format!(
                            "{}:{}:{}",
                            dependency.group_id,
                            dependency.artifact_id,
                            dependency.version.as_deref().unwrap_or("-")
                        ));
                    }
                }
            }
        }
        if here > 0 {
            with += 1;
            found += here;
        }
    });
    println!("POMs read: {poms}");
    println!("POMs with configuration coordinates: {with}");
    println!("coordinates found: {found}");
    println!("--- a sample ---");
    sample.sort();
    sample.dedup();
    for entry in &sample {
        println!("  {entry}");
    }
}

fn visit(path: &Path, seen: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit(&path, seen);
        } else if path.extension().is_some_and(|e| e == "pom") {
            seen(&path);
        }
    }
}
