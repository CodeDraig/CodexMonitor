{
  description = "CodexMonitor native Rust agent workbench";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        manifest = builtins.fromTOML (builtins.readFile ./src-tauri/Cargo.toml);
        linuxPackages = pkgs.lib.optionals pkgs.stdenv.isLinux [
          pkgs.alsa-lib
          pkgs.systemd
          pkgs.libxkbcommon
          pkgs.fontconfig
          pkgs.wayland
          pkgs.vulkan-loader
          pkgs.xorg.libX11
          pkgs.xorg.libXcursor
          pkgs.xorg.libXi
          pkgs.xorg.libXrandr
        ];
        buildPackages = [
          pkgs.cmake pkgs.llvmPackages.libclang pkgs.perl pkgs.pkg-config
        ];
        appPackage = pkgs.rustPlatform.buildRustPackage {
          pname = "codex-monitor";
          version = manifest.package.version;
          src = ./.;
          cargoRoot = "src-tauri";
          buildAndTestSubdir = "src-tauri";
          cargoLock = {
            lockFile = ./src-tauri/Cargo.lock;
            outputHashes = {
              "fix-path-env-0.0.0" = "sha256-UygkxJZoiJlsgp8PLf1zaSVsJZx1GGdQyTXqaFv3oGk=";
            };
          };
          nativeBuildInputs = buildPackages ++ [ pkgs.makeWrapper ];
          nativeCheckInputs = [ pkgs.git pkgs.python3 ];
          buildInputs = [ pkgs.openssl ] ++ linuxPackages;
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
          WHISPER_DONT_GENERATE_BINDINGS = "1";
          installPhase = ''
            runHook preInstall
            mkdir -p $out/bin $out/share/icons/hicolor/256x256/apps
            for binary in codex_monitor_native codex_monitor_daemon codex_monitor_daemonctl; do
              install -m755 target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release/$binary $out/bin/
            done
            ln -s codex_monitor_native $out/bin/codex-monitor
            install -m644 src-tauri/icons/icon.png $out/share/icons/hicolor/256x256/apps/codex-monitor.png
            runHook postInstall
          '' + pkgs.lib.optionalString pkgs.stdenv.isLinux ''
            wrapProgram $out/bin/codex_monitor_native \
              --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath linuxPackages}"
          '';
        };
      in {
        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.cargo pkgs.clang pkgs.git pkgs.openssl pkgs.python3
            pkgs.rust-analyzer pkgs.rustc pkgs.rustfmt pkgs.rustPlatform.rustLibSrc
          ] ++ buildPackages ++ linuxPackages;
          shellHook = ''
            export RUST_SRC_PATH=${pkgs.rustPlatform.rustLibSrc}
            export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"
          '' + pkgs.lib.optionalString pkgs.stdenv.isLinux ''
            export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath linuxPackages}:$LD_LIBRARY_PATH"
          '';
        };
        formatter = pkgs.alejandra;
        packages.default = appPackage;
      });
}
