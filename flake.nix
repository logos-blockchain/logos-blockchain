{
  description = "Development environment for Logos blockchain node.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    rust-overlay = {
      url = "github:oxalica/rust-overlay/40b0a3a193e0840c76174b4a322874c8f6dd0a63";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    crane.url = "github:ipetkov/crane";

    # Must stay in sync with the lbc-* tags in Cargo.toml.
    # Pinned to the commit right after the v0.5.3 tag (127626881faa975aa8e9868422cf6bbb08fcb512):
    # the tag itself predates the CI job that registers its Nix hashes in
    # circuits-nix-hashes.json, so building directly off the tag fails with
    # `attribute '"0.5.3"' missing`. This commit adds that entry without
    # otherwise changing the source.
    logos-blockchain-circuits = {
      url = "github:logos-blockchain/logos-blockchain-circuits/2846ee7a4cfa24458bb8063412ab2e753b344d2f";
    };

    # Must stay in sync with the rust-rapidsnark rev in Cargo.toml.
    rust-rapidsnark = {
      url = "github:logos-blockchain/logos-blockchain-rust-rapidsnark/e91187f8ccb5bbfc7bb00dac88169112428da78f";
    };
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      crane,
      logos-blockchain-circuits,
      rust-rapidsnark,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];

      forAll = nixpkgs.lib.genAttrs systems;

      mkPkgs =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

      rustVersion = "1.96.0";
    in
    {
      packages = forAll (
        system:
        let
          pkgs = mkPkgs system;
          rustToolchain = pkgs.rust-bin.stable.${rustVersion}.default;
          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
          src = pkgs.lib.cleanSourceWith {
            src = craneLib.path ./.;
            filter = path: type:
              (pkgs.lib.hasSuffix "nodes/node/binary/src/config/deployment/settings.yaml" path) ||
              (craneLib.filterCargoSources path type);
          };
          crateName = craneLib.crateNameFromCargoToml { inherit src; };

          commonArgs = {
            pname = "logos-blockchain-c";
            cargoExtraArgs = "-p logos-blockchain-c";
            version = crateName.version;
            doCheck = false;

            inherit src;

            buildInputs = [ pkgs.openssl ]
              ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ];
            nativeBuildInputs = [
              pkgs.pkg-config
              pkgs.clang
              pkgs.llvmPackages.libclang.lib
            ];
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            LBC_ROOT_DIR = logos-blockchain-circuits.packages.${system}.default;
            RAPIDSNARK_LIB_DIR = rust-rapidsnark.packages.${system}.rapidsnark;
          } // pkgs.lib.optionalAttrs pkgs.stdenv.isDarwin {
            RUSTFLAGS = "-L ${pkgs.libiconv}/lib";
          };

          logosBlockchainDependencies = craneLib.buildDepsOnly (commonArgs);

          logosBlockChainC = craneLib.buildPackage (
            commonArgs
            // {
              inherit logosBlockchainDependencies;

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
              pkgs.rust-bin.stable.${rustVersion}.default
              pkgs.clang
              pkgs.llvmPackages.libclang
              pkgs.openssl.dev
            ];
            shellHook = ''
              export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"
              export LBC_ROOT_DIR=${logos-blockchain-circuits.packages.${system}.default}
              export RAPIDSNARK_LIB_DIR=${rust-rapidsnark.packages.${system}.rapidsnark}
            '';
          };
        }
      );
    };
}
