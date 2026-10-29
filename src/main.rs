use std::io;
use std::path::Path;

mod entry;
mod indexer;
mod search;

use entry::FsEntry;
use indexer::read_dir;
use search::search;

fn main() {
    let mut files: Vec<FsEntry> = Vec::new();
    let mut input = String::new();

    println!("Enter directory to index:");
    let mut directory = String::new();

    io::stdin()
        .read_line(&mut directory)
        .expect("Failed to read directory");

    let directory = directory.trim();
    let path = Path::new(directory);

    if !path.is_dir() {
        eprintln!("Invalid directory: {directory}");
        return;
    }

    read_dir(path, &mut files);
    println!("\nTotal entries: {}", files.len());

    loop {
        println!("Search (or :q to quit):");

        input.clear();
        io::stdin()
            .read_line(&mut input)
            .expect("Failed to read line");
        let input = input.trim();

        if input.is_empty() {
            continue;
        }
        if input.eq_ignore_ascii_case(":q") {
            break;
        }

        let results = search(input, &files);

        for entry in &results {
            entry.info();
        }

        println!("Found {} entries", results.len());
    }
}
