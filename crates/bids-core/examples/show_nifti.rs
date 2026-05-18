fn main() {
    let path = std::env::args().nth(1).expect("path");
    let file = std::fs::File::open(path).expect("open NIfTI file");
    let header = bids_core::nifti::read_header(file);
    println!("{:#?}", header);
}
