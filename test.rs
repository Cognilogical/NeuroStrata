use std::path::Path;
fn main() {
    let p1 = Path::new("/home/kenton/Documents/NeuroStrata");
    let p2 = Path::new("/home/kenton/Documents/NeuroStrata/");
    println!("p1: {:?}", p1.file_name());
    println!("p2: {:?}", p2.file_name());
    println!("p1 comp: {:?}", p1.components().last());
    println!("p2 comp: {:?}", p2.components().last());
}
