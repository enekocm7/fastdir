# nixpkgs package: pkgs/by-name/fa/fastdir/package.nix.
#
# Unlike ../package.nix (the flake's, built from this checkout) this fetches
# the release tag. The hashes are placeholders: run
# `packaging/nix/update-nixpkgs.sh <version>` for the release being submitted
# (0.3.3 or later; older tags lack packaging/linux). Once it is in nixpkgs,
# r-ryantm keeps it updated (updateScript).
{
  lib,
  rustPlatform,
  fetchFromGitHub,
  pkg-config,
  makeWrapper,
  fontconfig,
  freetype,
  libxkbcommon,
  wayland,
  vulkan-loader,
  libx11,
  libxcb,
  nix-update-script,
}:

let
  # Opened with dlopen at run time, so they are not in the binary's RUNPATH.
  runtimeLibs = [
    vulkan-loader
    wayland
    libxkbcommon
    libx11
    libxcb
  ];
in
rustPlatform.buildRustPackage (finalAttrs: {
  pname = "fastdir";
  version = "0.3.2";

  src = fetchFromGitHub {
    owner = "enekocm7";
    repo = "fastdir";
    tag = "v${finalAttrs.version}";
    hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
  };

  cargoHash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];

  buildInputs = [
    fontconfig
    freetype
  ]
  ++ runtimeLibs;

  # The GUI-free test suite runs upstream; the GUI tests need a display.
  doCheck = false;

  postInstall = ''
    install -Dm644 packaging/linux/io.github.enekocm7.FastDir.desktop \
      -t $out/share/applications
    install -Dm644 packaging/linux/io.github.enekocm7.FastDir.metainfo.xml \
      -t $out/share/metainfo
    install -Dm644 assets/logo.svg \
      $out/share/icons/hicolor/scalable/apps/io.github.enekocm7.FastDir.svg
  '';

  postFixup = ''
    wrapProgram $out/bin/fastdir \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}
  '';

  passthru.updateScript = nix-update-script { };

  meta = {
    description = "Fast disk usage analyzer with a treemap";
    longDescription = ''
      FastDir scans a drive or folder on every CPU core and shows where the
      space went: a sortable directory tree, a per-extension breakdown and a
      cushion-shaded treemap. It also ships fastdir-cli, a headless scanner
      with JSON/CSV output and an MCP server.
    '';
    homepage = "https://github.com/enekocm7/fastdir";
    changelog = "https://github.com/enekocm7/fastdir/releases/tag/v${finalAttrs.version}";
    license = lib.licenses.gpl3Plus;
    maintainers = with lib.maintainers; [ enekocm7 ];
    mainProgram = "fastdir";
    platforms = lib.platforms.linux;
  };
})
