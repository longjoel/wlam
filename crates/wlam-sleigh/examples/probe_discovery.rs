fn main() {
    let path = std::env::args().nth(1).expect("PE path");
    let bytes = std::fs::read(path).expect("read PE");
    let pe = wlam_pe::Pe::parse(&bytes).expect("parse PE");
    let program = wlam_sleigh::with_big_stack(move || wlam_sleigh::discover::discover(&pe, 4096));
    println!(
        "passes={} blocks={} pending={} unresolved={}",
        program.passes(),
        program.block_count(),
        program.pending().len(),
        program.unresolved_indirect().len()
    );
    for (address, reason) in program.pending().iter().take(64) {
        println!("pending {address:#010x} {reason}");
    }
}
