{
  description = "Development environment for Logos blockchain node.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # The Logos toolchain. Windows builds use it to stay compatible with the rest of Logos.
    logos-nix.follows = "logos-blockchain-circuits/logos-nix";

    rust-overlay = {
      url = "github:oxalica/rust-overlay/8bbfc858c7b85f5e6ed6d6cc201ffcee1bb08a9b";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    crane.url = "github:ipetkov/crane";

    # Must stay in sync with the lbc-* tags in Cargo.toml.
    logos-blockchain-circuits = {
      url = "github:logos-blockchain/logos-blockchain-circuits/v0.8.0-rc.2";
    };

    logos-blockchain-rapidsnark.follows = "logos-blockchain-circuits/logos-blockchain-rapidsnark"; # Must stay in sync with the rust-rapidsnark rev in Cargo.toml.
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      crane,
      logos-blockchain-circuits,
      logos-blockchain-rapidsnark,
      logos-nix,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];

      forAll = nixpkgs.lib.genAttrs systems;

      mkPkgs =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

      rustVersion = "1.100.0";
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
          # Directories whose files are embedded too: the era files the
          # default deployment includes.
          assetDirs = [
            "nodes/node/binary/src/config/deployment/era/schedule/"
          ];
          src = pkgs.lib.cleanSourceWith {
            src = craneLib.path ./.;
            filter = path: type:
              (pkgs.lib.any (suffix: pkgs.lib.hasSuffix suffix path) assetSuffixes) ||
              (pkgs.lib.any (dir: pkgs.lib.hasInfix dir path) assetDirs) ||
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
              pkgs.git
            ];
            # The source is a git-less snapshot; give `git rev-parse HEAD` the flake's rev.
            preBuild = pkgs.lib.optionalString (self ? rev) ''
              if [ ! -e .git ]; then
                git init -q
                echo ${self.rev} > .git/HEAD
              fi
            '';
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            LBC_ROOT_DIR = logos-blockchain-circuits.packages.${system}.default;
            RAPIDSNARK_LIB_DIR = "${logos-blockchain-rapidsnark.packages.${system}.default}/lib";
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

          # Windows is cross-built from x86_64-linux with logos-nix's MinGW.
          windowsTarget = "x86_64-pc-windows-gnu";
          windowsPkgs = logos-nix.legacyPackages.${system}.pkgsWindows;
          windowsCc = windowsPkgs.stdenv.cc;
          windowsCcPrefix = "${windowsCc}/bin/${windowsCc.targetPrefix}";
          windowsCraneLib = (crane.mkLib pkgs).overrideToolchain (
            pkgs.rust-bin.stable.${rustVersion}.default.override { targets = [ windowsTarget ]; }
          );

          # The circuits flake's `packages.x86_64-windows` does not evaluate, so fetch its Windows release here.
          circuitsVersion = logos-blockchain-circuits.packages.${system}.default.version;
          circuitsHashes = builtins.fromJSON (builtins.readFile "${logos-blockchain-circuits}/circuits_nix_hashes.json");
          windowsCircuits = pkgs.stdenvNoCC.mkDerivation {
            pname = "logos-blockchain-circuits-windows-x86_64";
            version = circuitsVersion;
            src = pkgs.fetchurl {
              url = "https://github.com/logos-blockchain/logos-blockchain-circuits/releases/download/v${circuitsVersion}/logos-blockchain-circuits-v${circuitsVersion}-windows-x86_64.tar.gz";
              hash = circuitsHashes.${circuitsVersion}.x86_64-windows;
            };
            dontConfigure = true;
            dontBuild = true;
            dontFixup = true;
            installPhase = "cp -r . $out";
          };

          windowsArgs = commonArgs // {
            pname = "logos-blockchain-c-windows-x86_64";
            CARGO_BUILD_TARGET = windowsTarget;
            buildInputs = [ ];
            nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ windowsCc ];

            CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "${windowsCcPrefix}gcc";
            CC_x86_64_pc_windows_gnu = "${windowsCcPrefix}gcc";
            CXX_x86_64_pc_windows_gnu = "${windowsCcPrefix}g++";
            AR_x86_64_pc_windows_gnu = "${windowsCcPrefix}ar";
            # logos-nix's MinGW uses mcfgthread, so it ships no `sched.h`; aws-lc-sys needs it.
            CFLAGS_x86_64_pc_windows_gnu = "-I${windowsPkgs.windows.pthreads}/include";
            # Rust expects pthreads, but logos-nix's MinGW uses mcfgthread.
            CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS = builtins.concatStringsSep " " [
              "-L native=${windowsPkgs.windows.pthreads}/lib"
              "-C link-arg=-l:libmcfgthread.a"
              "-C link-arg=-lntdll"
              "-C link-arg=-lkernel32"
            ];

            LBC_ROOT_DIR = windowsCircuits;
            RAPIDSNARK_LIB_DIR = "${logos-blockchain-rapidsnark.packages.${system}.windows-x86_64}/lib";
          };

          logosBlockChainCWindows = windowsCraneLib.buildPackage (
            windowsArgs
            // {
              cargoArtifacts = windowsCraneLib.buildDepsOnly windowsArgs;

              postInstall = ''
                mkdir -p $out/include
                cp c-bindings/logos_blockchain.h $out/include/
              '';
            }
          );
        in
        {
          logos-blockchain-c = logosBlockChainC;
          default = logosBlockChainC;
        }
        // pkgs.lib.optionalAttrs (system == "x86_64-linux") {
          logos-blockchain-c-windows-x86_64 = logosBlockChainCWindows;
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
              export RAPIDSNARK_LIB_DIR=${logos-blockchain-rapidsnark.packages.${system}.default}/lib
            '';
          };
        }
      );
    };
}
