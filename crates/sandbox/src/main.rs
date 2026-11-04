//! `rustcloud-sandbox`: the same as `rustcloud sandbox`, on its own — for
//! this crate's tests and for trying a tool by hand.

fn main() {
    rustcloud_sandbox::main(std::env::args_os().skip(1).collect())
}
