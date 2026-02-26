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
          pkgs = mkPkgs system;
          rustToolchain = pkgs.rust-bin.stable.latest.default;
          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
          src = craneLib.cleanCargoSource ./.;

          commonArgs = {
            pname = "logos-blockchain-c";
            cargoExtraArgs = "-p logos-blockchain-c";
            version = "0.1.0";

            inherit src;

            buildInputs = [ pkgs.openssl ]
              ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ];
            nativeBuildInputs = [
              pkgs.pkg-config
              pkgs.clang
              pkgs.llvmPackages.libclang.lib
            ];
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            LOGOS_BLOCKCHAIN_CIRCUITS = logos-blockchain-circuits.packages.${system}.default;
          } // pkgs.lib.optionalAttrs pkgs.stdenv.isDarwin {
            RUSTFLAGS = "-L ${pkgs.libiconv}/lib";
          };

          logosBlockchainDependencies = craneLib.buildDepsOnly (commonArgs);

          logosBlockChainC = craneLib.buildPackage (
            commonArgs
            // {
              inherit logosBlockchainDependencies;

              postInstall = ''
                mkdir -p $out/circuits $out/include
                cp -r ${logos-blockchain-circuits.packages.${system}.default}/* $out/circuits/
                cp c-bindings/logos_blockchain.h $out/include/

                # Files copied from the Nix store are read-only.
                # Crane modifies files in $out after install, so they must be writable during the build.
                # Nix makes the final output read-only again, so this is safe.
                chmod -R u+w $out/circuits
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
