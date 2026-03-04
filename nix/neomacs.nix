{ lib
, stdenv
, craneLib
, pkg-config
, autoconf
, automake
, texinfo
, llvmPackages
, ncurses
, gnutls
, zlib
, libxml2
, fontconfig
, freetype
, harfbuzz
, cairo
, gtk4
, glib
, graphene
, pango
, gdk-pixbuf
, gst_all_1
, libsoup_3
, glib-networking
, libjpeg
, libtiff
, giflib
, libpng
, librsvg
, libwebp
, dbus
, sqlite
, tree-sitter
, gmp
, makeWrapper
# Linux-only deps (optional on darwin)
, libselinux ? null
, libgccjit ? null
, libGL ? null
, libxkbcommon ? null
, mesa ? null
, libdrm ? null
, libgbm ? null
, libva ? null
, wayland ? null
, wayland-protocols ? null
, wpewebkit ? null
, libwpe ? null
, libwpe-fdo ? null
, weston ? null
, vulkan-loader ? null
}:

let
  isLinux = stdenv.isLinux;
  isDarwin = stdenv.isDarwin;
  enableVideo = isLinux;

  # Rust source root. Keep sibling crates available for path deps:
  # neomacs-display -> ../neovm-core -> ../neovm-host-abi
  rustSrc = lib.cleanSourceWith {
    src = ../.;
    filter = path: type:
      (craneLib.filterCargoSources path type)
      || (lib.hasInfix "/assets/" path)
      || (lib.hasInfix "/shaders/" path)
      || (lib.hasInfix "/icons/" path)
      || (lib.hasSuffix ".wgsl" path)
      || (lib.hasSuffix ".png" path)
      || (lib.hasSuffix ".svg" path);
  };

  # Common arguments shared between deps-only and full builds
  commonArgs = {
    pname = "neomacs-display";
    version = "0.1.0";

    src = rustSrc;
    cargoLock = ../Cargo.lock;

    nativeBuildInputs = [
      pkg-config
      llvmPackages.libclang
    ];

    buildInputs = [
      gtk4
      glib
      graphene
      pango
      cairo
      gdk-pixbuf
      libsoup_3
    ]
    ++ lib.optionals enableVideo [
      gst_all_1.gstreamer
      gst_all_1.gst-plugins-base
      gst_all_1.gst-plugins-bad
    ]
    ++ lib.optionals isLinux [
      libva
      libGL
      libxkbcommon
      wayland
      wayland-protocols
      wpewebkit
      libwpe
      libwpe-fdo
    ];

    PKG_CONFIG_PATH = lib.makeSearchPath "lib/pkgconfig" ([
      gtk4.dev
      glib.dev
      graphene
      pango.dev
      cairo.dev
      gdk-pixbuf.dev
      libsoup_3.dev
    ]
    ++ lib.optionals enableVideo [
      gst_all_1.gstreamer.dev
      gst_all_1.gst-plugins-base.dev
      gst_all_1.gst-plugins-bad.dev
    ]
    ++ lib.optionals isLinux [
      libva
      libGL.dev
      libxkbcommon.dev
      wayland.dev
      wpewebkit
      libwpe
      libwpe-fdo
    ]);

    LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";

    BINDGEN_EXTRA_CLANG_ARGS = builtins.concatStringsSep " " ([
      "-isystem ${glib.dev}/include/glib-2.0"
      "-isystem ${glib.out}/lib/glib-2.0/include"
      "-isystem ${gtk4.dev}/include/gtk-4.0"
      "-isystem ${cairo.dev}/include/cairo"
      "-isystem ${pango.dev}/include/pango-1.0"
      "-isystem ${graphene}/include/graphene-1.0"
      "-isystem ${gdk-pixbuf.dev}/include/gdk-pixbuf-2.0"
    ]
    ++ lib.optionals isLinux [
      "-isystem ${stdenv.cc.libc.dev}/include"
      "-isystem ${libxkbcommon.dev}/include"
      "-isystem ${wayland.dev}/include"
      "-isystem ${libGL.dev}/include"
    ]);

    cargoExtraArgs = if isLinux
      then "--manifest-path neomacs-display/Cargo.toml --lib"
      else "--manifest-path neomacs-display/Cargo.toml --lib --no-default-features --features neo-term,core-backend-emacs-c";
    doCheck = false;
  };

  # Step 1: Build only dependencies (cached when Cargo.lock unchanged)
  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

  # Step 2: Build the actual crate (reuses cached deps)
  neomacs-display = craneLib.buildPackage (commonArgs // {
    inherit cargoArtifacts;

    postInstall = ''
      mkdir -p $out/include
      cp -r neomacs-display/include/* $out/include/ || true
    '';
  });

in stdenv.mkDerivation {
  pname = "neomacs";
  version = "30.0.50-neomacs";
  enableParallelBuilding = true;

  src = ./..;

  nativeBuildInputs = [
    pkg-config
    autoconf
    automake
    texinfo
    makeWrapper
  ];

  buildInputs = [
    ncurses
    gnutls
    zlib
    libxml2
    fontconfig
    freetype
    harfbuzz
    cairo
    gtk4
    glib
    graphene
    pango
    gdk-pixbuf
    gst_all_1.gstreamer
    gst_all_1.gst-plugins-base
    gst_all_1.gst-plugins-good
    gst_all_1.gst-plugins-bad
    gst_all_1.gst-plugins-ugly
    gst_all_1.gst-libav
    gst_all_1.gst-plugins-rs
    libsoup_3
    glib-networking
    libjpeg
    libtiff
    giflib
    libpng
    librsvg
    libwebp
    dbus
    sqlite
    tree-sitter
    gmp
    # Link against our Rust library
    neomacs-display
  ]
  ++ lib.optionals isLinux [
    libselinux
    libgccjit
    libGL
    libxkbcommon
    mesa
    libdrm
    libgbm
    wayland
    wayland-protocols
    wpewebkit
    libwpe
    libwpe-fdo
    weston
  ];

  # Point to the pre-built Rust library
  NEOMACS_DISPLAY_LIB = "${neomacs-display}/lib";
  NEOMACS_DISPLAY_INCLUDE = "${neomacs-display}/include";

  preConfigure = ''
    echo "Using pre-built neomacs-display from: ${neomacs-display}"
    export NEOMACS_DISPLAY_LIB="${neomacs-display}/lib"
    export NEOMACS_DISPLAY_INCLUDE="${neomacs-display}/include"
    ./autogen.sh
  '';

  configureFlags = [
    "--with-neomacs"
    "--with-native-compilation=no"  # Disabled temporarily due to libgccjit nix issue
    "--with-gnutls"
    "--with-xml2"
    "--with-tree-sitter"
    "--with-modules"
    "CFLAGS=-I${neomacs-display}/include"
    "LDFLAGS=-L${neomacs-display}/lib"
  ];

  # Set up environment for WPE WebKit (Linux only)
  preBuild = lib.optionalString isLinux ''
    export WPE_BACKEND_LIBRARY="${libwpe-fdo}/lib/libWPEBackend-fdo-1.0.so"
    export GIO_MODULE_DIR="${glib-networking}/lib/gio/modules"
  '';

  # Wrap the binary with required environment variables
  postInstall = if isLinux then ''
    wrapProgram $out/bin/emacs \
      --set WPE_BACKEND_LIBRARY "${libwpe-fdo}/lib/libWPEBackend-fdo-1.0.so" \
      --set GIO_MODULE_DIR "${glib-networking}/lib/gio/modules" \
      --set WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS "1" \
      --prefix PATH : "${wpewebkit}/libexec/wpe-webkit-2.0"
  '' else ''
    wrapProgram $out/bin/emacs \
      --set GIO_MODULE_DIR "${glib-networking}/lib/gio/modules"
  '';

  setupHook = builtins.toFile "neomacs-setup-hook.sh" ''
    addToEmacsLoadPath() {
      local lispDir="$1"
      if [[ -d $lispDir && ''${EMACSLOADPATH-} != *"$lispDir":* ]]; then
        # A trailing ":" keeps Emacs's default search semantics intact.
        export EMACSLOADPATH="$lispDir:''${EMACSLOADPATH-}"
      fi
    }

    addToEmacsNativeLoadPath() {
      local nativeDir="$1"
      if [[ -d $nativeDir && ''${EMACSNATIVELOADPATH-} != *"$nativeDir":* ]]; then
        export EMACSNATIVELOADPATH="$nativeDir:''${EMACSNATIVELOADPATH-}"
      fi
    }

    addEmacsVars() {
      addToEmacsLoadPath "$1/share/emacs/site-lisp"

      if [ -n "''${addEmacsNativeLoadPath:-}" ]; then
        addToEmacsNativeLoadPath "$1/share/emacs/native-lisp"
      fi
    }

    addEnvHooks "$targetOffset" addEmacsVars
  '';

  meta = with lib; {
    description = "Neomacs - GPU-accelerated Emacs written in Rust with a modern, multithreaded architecture";
    homepage = "https://github.com/eval-exec/neomacs";
    license = licenses.gpl3Plus;
    platforms = platforms.linux ++ platforms.darwin;
    mainProgram = "emacs";
    maintainers = [ ];
  };
}
