# FastDir built from this checkout. Used by the repository's flake.nix; a
# nixpkgs submission would fetch the release tag instead of `src` below.
{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  fontconfig,
  freetype,
  libxkbcommon,
  wayland,
  vulkan-loader,
  xorg,
}:

let
  workspace = lib.importTOML ../../Cargo.toml;
  # Opened with dlopen at run time, so they are not in the binary's RUNPATH.
  runtimeLibs = [
    vulkan-loader
    wayland
    libxkbcommon
    xorg.libX11
    xorg.libxcb
  ];
in
rustPlatform.buildRustPackage {
  pname = "fastdir";
  inherit (workspace.workspace.package) version;

  src = lib.fileset.toSource {
    root = ../..;
    fileset = lib.fileset.unions [
      ../../Cargo.toml
      ../../Cargo.lock
      ../../build.rs
      ../../src
      ../../crates
      ../../assets
      ../../packaging/linux
    ];
  };

  cargoLock.lockFile = ../../Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];
  buildInputs = [
    fontconfig
    freetype
  ]
  ++ runtimeLibs;

  # The test suite is run by the project's CI; the GUI tests need a display.
  doCheck = false;

  postInstall = ''
    install -Dm644 packaging/linux/io.github.enekocm7.FastDir.desktop \
      $out/share/applications/io.github.enekocm7.FastDir.desktop
    install -Dm644 packaging/linux/io.github.enekocm7.FastDir.metainfo.xml \
      $out/share/metainfo/io.github.enekocm7.FastDir.metainfo.xml
    install -Dm644 assets/logo.svg \
      $out/share/icons/hicolor/scalable/apps/io.github.enekocm7.FastDir.svg
  '';

  postFixup = ''
    wrapProgram $out/bin/fastdir \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}
  '';

  passthru = { inherit runtimeLibs; };

  meta = {
    description = "Blazing fast disk usage analyzer with a WinDirStat-style treemap";
    homepage = "https://github.com/enekocm7/fastdir";
    license = lib.licenses.gpl3Plus;
    mainProgram = "fastdir";
    platforms = lib.platforms.linux;
  };
}
