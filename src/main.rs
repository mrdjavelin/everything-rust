mod app;
mod core;
mod database;
mod indexing;
mod journal;
mod platform;
mod roots;
mod services;
mod ui;

fn main() -> std::io::Result<()> {
    app::run()
}
