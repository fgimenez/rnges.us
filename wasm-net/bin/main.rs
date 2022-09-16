use env_logger::{Builder, Env};
use futures::executor::block_on;

fn main() {
    Builder::from_env(Env::default().default_filter_or("info")).init();
    let to_dial = std::env::args().nth(1);
    block_on(wasm_net::service(None, to_dial));
}
