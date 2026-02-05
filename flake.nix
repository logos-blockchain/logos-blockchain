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
      url = "github:logos-blockchain/logos-blockchain-circuits";
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

          logosBlockchainDependencies = craneLib.buildDepsOnly (
            commonArgs
            // {
              pname = "logos-blockchain";
              version = "0.1.0";
            }
          );

          logosBlockChainC = craneLib.buildPackage (
            commonArgs
            // {
              inherit logosBlockchainDependencies;
              pname = "logos-blockchain-c";
              version = "0.1.0";
              cargoExtraArgs = "-p logos-blockchain-c";

              postInstall = ''
                mkdir -p $out/include
                cp c-bindings/logos_blockchain.h $out/include/
              '' + pkgs.lib.optionalString pkgs.stdenv.isDarwin ''
                install_name_tool -id @rpath/liblogos_blockchain.dylib $out/lib/liblogos_blockchain.dylib
              '';
            }
          );
        in
        {
          logos-blockchain-c = logosBlockChainC;
          default = logosBlockChainC;
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
