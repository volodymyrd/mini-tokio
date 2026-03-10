mod executor;

use std::thread;
use std::time::Duration;

use crate::executor::block_on;

fn main() {
    println!("Lazy execution...");

    let lazy_feature = async {
        println!("Future polled to the executor!");
        42
    };

    println!("Future created but nothing printed yet!");

    thread::sleep(Duration::from_secs(1));
    let result = block_on(lazy_feature);
    println!("Result: {result}\n");
}
