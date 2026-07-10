// DCENT_axe root build script
// Ensures ESP-IDF native build system is properly configured

fn main() {
    // esp-idf-sys handles the actual ESP-IDF build via its own build.rs
    // This file exists for any workspace-level build configuration
    embuild::espidf::sysenv::output();
}
