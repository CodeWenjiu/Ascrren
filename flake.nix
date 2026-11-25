{
  description = "Rust development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          targets = [ "wasm32-unknown-unknown" ];
        };
      in
      with pkgs;
      {
        devShell = mkShell rec {
          buildInputs = with pkgs; [
            rustToolchain
            rust-analyzer
            cargo-watch
            cargo-edit
            cargo-leptos
            binaryen
            wasm-bindgen-cli
            sccache
          ];

          # 显式声明，避免从父进程（例如在别的项目 direnv 会话里启动的编辑器）
          # 继承到指向其他项目的 RUSTC_WRAPPER / SCCACHE_DIR。
          shellHook = ''
            export RUSTC_WRAPPER=sccache
            export SCCACHE_DIR="$PWD/.sccache"
          '';

          LD_LIBRARY_PATH = "${lib.makeLibraryPath buildInputs}";
        };
      }
    );
}
