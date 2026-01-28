{
  description = "Development environment for Logos blockchain node.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    crane.url = "github:ipetkov/crane";

    logos-blockchain-circuits = {
      url = "github:logos-blockchain/logos-blockchain-circuits?ref=feat/nixify";
    };
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      crane,
      logos-blockchain-circuits,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
        "x86_64-windows"
      ];

      forAll = nixpkgs.lib.genAttrs systems;

      mkPkgs =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
    in
    {
      packages = forAll (
        system:
        let
          src = craneLib.cleanCargoSource ./.;
          pkgs = mkPkgs system;

          rustToolchain = pkgs.rust-bin.stable.latest.default;
          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

          commonArgs = {
            inherit src;
            buildInputs = [ pkgs.openssl ];
            nativeBuildInputs = [
              pkgs.pkg-config
              pkgs.clang
              pkgs.llvmPackages.libclang.lib
            ];
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            LOGOS_BLOCKCHAIN_CIRCUITS = logos-blockchain-circuits.packages.${system}.default;
          };

          cargoArtifacts = craneLib.buildDepsOnly (
            commonArgs
            // {
              pname = "logos-blockchain-deps";
              version = "0.1.0";
            }
          );

          logos-blockchain-c = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              pname = "logos-blockchain-c";
              version = "0.1.0";
              cargoExtraArgs = "-p logos-blockchain-c";

              postInstall = ''
                mkdir -p $out/include
                cp c-bindings/lib_logos_blockchain.h $out/include/
              '';
            }
          );
        in
        {
          inherit logos-blockchain-c;
        }
      );

      devShells = forAll (
        system:
        let
          pkgs = mkPkgs system;
        in
        {
          research = pkgs.mkShell {
            name = "research";
            buildInputs = [
              pkgs.pkg-config
              pkgs.rust-bin.stable.latest.default
              pkgs.clang
              pkgs.llvmPackages.libclang
              pkgs.openssl.dev
            ];
            shellHook = ''
              export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"
              export LOGOS_BLOCKCHAIN_CIRCUITS=${logos-blockchain-circuits.packages.${system}.default}
            '';
          };
        }
      );
    };
}
