{
  description = "xlr development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix.url = "github:nix-community/fenix";
  };

  outputs =
    {
      nixpkgs,
      fenix,
      ...
    }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      rustToolchain = fenix.packages.${system}.complete.toolchain;
      # Cargo needs a C linker for build scripts, tests, and examples.
      cargoInputs = [
        rustToolchain
        pkgs.stdenv.cc
      ];
      format = pkgs.writeShellApplication {
        name = "xlr-format";
        runtimeInputs = [ rustToolchain ];
        text = ''
          cargo fmt --all
        '';
      };
      formatCheck = pkgs.writeShellApplication {
        name = "xlr-format-check";
        runtimeInputs = [ rustToolchain ];
        text = ''
          cargo fmt --all -- --check
        '';
      };
      clippy = pkgs.writeShellApplication {
        name = "xlr-clippy";
        runtimeInputs = cargoInputs;
        text = ''
          cargo clippy --workspace --all-targets -- -D warnings
        '';
      };
      test = pkgs.writeShellApplication {
        name = "xlr-test";
        runtimeInputs = cargoInputs;
        text = ''
          cargo test --workspace
        '';
      };
      package = pkgs.writeShellApplication {
        name = "xlr-package";
        runtimeInputs = cargoInputs;
        text = ''
          cargo package --workspace --allow-dirty
        '';
      };
      ci = pkgs.writeShellApplication {
        name = "xlr-ci";
        text = ''
          ${formatCheck}/bin/xlr-format-check
          ${clippy}/bin/xlr-clippy
          ${test}/bin/xlr-test
          ${package}/bin/xlr-package
        '';
      };
      mkApp = package: name: description: {
        type = "app";
        program = "${package}/bin/${name}";
        meta = { inherit description; };
      };
    in
    {
      devShells.${system}.default = pkgs.mkShell {
        packages = [
          rustToolchain
          pkgs.cargo-nextest
          pkgs.mold
          pkgs.pkg-config
        ];

        RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
      };

      formatter.${system} = format;

      apps.${system} = {
        fmt-check = mkApp formatCheck "xlr-format-check" "Check Rust formatting";
        clippy = mkApp clippy "xlr-clippy" "Run strict Clippy";
        test = mkApp test "xlr-test" "Run all Rust tests";
        package = mkApp package "xlr-package" "Build and verify the publish archives";
        ci = mkApp ci "xlr-ci" "Run the complete local Rust QA gate";
      };
    };
}
