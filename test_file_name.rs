use std::path::Path;
fn main() {
    let p = Path::new("/home/kenton/Documents/NeuroStrata/");
    println!("file_name: {:?}", p.file_name());
}
