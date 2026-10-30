mod database;
mod entry;
mod indexer;
mod journal;
mod platform;
mod roots;
mod search;

use std::io;

use indexer::scan_root;
use roots::get_scan_roots;

fn reindex(connection: &mut rusqlite::Connection) -> bool {
    let roots = get_scan_roots();
    if roots.is_empty() {
        eprintln!("No accessible drives found.");
        return false;
    }

    let mut all_complete = true;

    for root in &roots {
        #[cfg(windows)]
        {
            match platform::windows_usn::sync_volume(connection, root) {
                Ok(()) => {
                    println!("Synchronized {} from the NTFS journal.", root.display());
                    continue;
                }
                Err(error) => {
                    eprintln!(
                        "USN synchronization failed for {}: {error}. Using filesystem scan.",
                        root.display()
                    );
                    if let Err(error) = database::invalidate_volume_sync_for_root(connection, root)
                    {
                        eprintln!(
                            "Could not invalidate journal state for {}: {error}",
                            root.display()
                        );
                        all_complete = false;
                        continue;
                    }
                }
            }
        }

        println!("Scanning {}...", root.display());
        let scan_id = match database::begin_scanned_root(connection) {
            Ok(scan_id) => scan_id,
            Err(error) => {
                eprintln!(
                    "Could not start fallback scan for {}: {error}",
                    root.display()
                );
                all_complete = false;
                continue;
            }
        };
        let scan_result = scan_root(root, |batch| {
            database::save_scanned_batch(connection, scan_id, batch)
                .map_err(|error| error.to_string())
        });
        match scan_result {
            Ok(summary) => {
                if let Err(error) =
                    database::finish_scanned_root(connection, root, scan_id, summary.complete)
                {
                    eprintln!("Failed to finalize scan of {}: {error}", root.display());
                    all_complete = false;
                    continue;
                }
                println!(
                    "Indexed {} entries from {} ({} skipped).{}",
                    summary.discovered,
                    root.display(),
                    summary.skipped,
                    if summary.complete {
                        " Scan completed."
                    } else {
                        " Partial scan; stale entries preserved."
                    }
                );
                if !summary.complete {
                    all_complete = false;
                }
            }
            Err(error) => {
                eprintln!("Fallback scan failed for {}: {error}", root.display());
                all_complete = false;
            }
        }
    }

    all_complete
}

fn main() {
    let db_path = database::get_database_path().expect("Failed to determine database path");

    let mut connection = database::open_database(db_path.to_str().expect("Invalid database path"))
        .expect("Failed to open database");
    let roots = get_scan_roots();

    if roots.is_empty() {
        eprintln!("No accessible drives found.");
        return;
    }

    println!("Detected {} drive(s):", roots.len());

    for root in &roots {
        println!("  {}", root.display());
    }

    reindex(&mut connection);

    let mut input = String::new();

    loop {
        println!("Search (or :reindex / :q):");
        input.clear();

        if io::stdin().read_line(&mut input).is_err() {
            eprintln!("Failed to read input.");
            break;
        }

        let query = input.trim();

        if query.is_empty() {
            continue;
        }

        if query.eq_ignore_ascii_case(":q") {
            break;
        }

        if query.eq_ignore_ascii_case(":reindex") {
            reindex(&mut connection);
            continue;
        }

        match database::search_entries(&connection, query) {
            Ok(results) => {
                for entry in &results {
                    entry.info();
                }

                println!("Found {} entries", results.len());
            }
            Err(err) => eprintln!("Search failed: {err}"),
        }
    }
}
