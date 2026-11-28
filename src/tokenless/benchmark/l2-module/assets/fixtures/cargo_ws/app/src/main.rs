//! L2 fixture: a successful build with two warnings.

fn main() {
    let scratch = 1;
    let total: u32 = dep01::value() + dep02::value() + dep03::value() + dep04::value() + dep05::value() + dep06::value() + dep07::value() + dep08::value() + dep09::value() + dep10::value() + dep11::value() + dep12::value();
    println!("total={total}");
}

fn unused_helper() -> u32 {
    7
}
