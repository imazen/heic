fn main() {
    use heic_core::HevcBackend;
    let b = heic_backend_vaapi::VaApiBackend::new();
    let avail = b.is_available();
    eprintln!("VaApiBackend::is_available() = {avail}");
    std::process::exit(if avail { 0 } else { 1 });
}
