fn main() {
    cc::Build::new()
        .cpp(true)
        .include("vendor")
        .file("vendor/MiniBpm.cpp")
        .file("vendor/minibpm_c.cpp")
        .flag_if_supported("-std=c++14")
        .flag_if_supported("-O2")
        .flag_if_supported("-fno-exceptions")
        .compile("minibpm");
}
