{
  description = "Development environment for Logos blockchain node.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    rust-overlay = {
      url = "github:oxalica/rust-overlay/9eccf73c5b810052f08aa77ae0548c383259f17f";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    crane.url = "github:ipetkov/crane";

    # Must stay in sync with the lbc-* tags in Cargo.toml.
    logos-blockchain-circuits = {
      url = "github:logos-blockchain/logos-blockchain-circuits/v0.5.6";
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

      rustVersion = "1.98.1";
    in
    {
      packages = forAll (
        system:
        let
          pkgs = mkPkgs system;
          rustToolchain = pkgs.rust-bin.stable.${rustVersion}.default;
          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
          # `craneLib.filterCargoSources` keeps only *.rs, *.toml, Cargo.lock and
          # .cargo/config, so any non-Rust file pulled in with include_str!/
          # include_bytes! has to be allowed through explicitly or the build
          # fails with "couldn't read ...: No such file or directory".
          assetSuffixes = [
            ".hex"
            "nodes/node/binary/src/config/deployment/settings.yaml"
          ];
          src = pkgs.lib.cleanSourceWith {
            src = craneLib.path ./.;
            filter = path: type:
              (pkgs.lib.any (suffix: pkgs.lib.hasSuffix suffix path) assetSuffixes) ||
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

          logosBlockchainDependencies = craneLib.buildDepsOnly commonArgs;

          logosBlockChainC = craneLib.buildPackage (
            commonArgs
            // {
              cargoArtifacts = logosBlockchainDependencies;

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
