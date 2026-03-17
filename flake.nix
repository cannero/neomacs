{
  description = "Neomacs - GPU-accelerated Emacs written in Rust with a modern, multithreaded architecture";

  nixConfig = {
    extra-substituters = [ "https://nix-wpe-webkit.cachix.org" ];
    extra-trusted-public-keys = [ "nix-wpe-webkit.cachix.org-1:ItCjHkz1Y5QcwqI9cTGNWHzcox4EqcXqKvOygxpwYHE=" ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    # Rust toolchain
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
    };

    # Crane for incremental Rust builds (caches deps separately from source)
    crane.url = "github:ipetkov/crane";

    # WPE WebKit standalone flake with Cachix binary cache
    # Do NOT use `inputs.nixpkgs.follows` here — the Cachix binary was built
    # with nix-wpe-webkit's own pinned nixpkgs, so follows would change the
    # derivation hash and cause a cache miss (rebuilding from source ~1 hour).
    nix-wpe-webkit = {
      url = "github:eval-exec/nix-wpe-webkit";
    };
  };

  outputs = { self, nixpkgs, rust-overlay, crane, nix-wpe-webkit }:
    let
      lib = nixpkgs.lib;

      supportedSystems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];

      forAllSystems = lib.genAttrs supportedSystems;

      # Create pkgs with overlays for each system
      pkgsFor = system: import nixpkgs {
        inherit system;
        overlays = [
          rust-overlay.overlays.default
          self.overlays.default
        ];
      };

    in {
      # Overlay that provides wpewebkit (Linux only) and rust toolchain
      overlays.default = final: prev: {
        # Rust toolchain from rust-toolchain.toml (with extra extensions)
        rust-neomacs = (final.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml).override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };
      } // (lib.optionalAttrs prev.stdenv.isLinux {
        # WPE WebKit from nix-wpe-webkit flake (with Cachix binary cache)
        # Only available on Linux — WPE WebKit does not support macOS.
        wpewebkit = nix-wpe-webkit.packages.${final.stdenv.hostPlatform.system}.wpewebkit;
      });

      # Development shell
      devShells = forAllSystems (system:
        let
          pkgs = pkgsFor system;
          isLinux = pkgs.stdenv.isLinux;
          isDarwin = pkgs.stdenv.isDarwin;
        in {
          default = pkgs.mkShell {
            name = "neomacs-dev";

            nativeBuildInputs = [
              # Rust toolchain
              pkgs.rust-neomacs
              pkgs.rust-cbindgen

              # Build tools
              pkgs.pkg-config
              pkgs.autoconf
              pkgs.automake
              pkgs.texinfo

              # For bindgen (generates Rust bindings from C headers)
              pkgs.llvmPackages.clang
            ];

            buildInputs = with pkgs; [
              # Standard Emacs build dependencies
              ncurses
              gnutls
              zlib
              libxml2

              # Font support
              fontconfig
              freetype
              harfbuzz

              # Cairo
              cairo

              # GLib for event loop integration
              glib

              # GStreamer for video support
              gst_all_1.gstreamer
              gst_all_1.gst-plugins-base
              gst_all_1.gst-plugins-good
              gst_all_1.gst-plugins-bad
              gst_all_1.gst-plugins-ugly
              gst_all_1.gst-libav
              gst_all_1.gst-plugins-rs

              # libsoup for HTTP
              libsoup_3

              # GLib networking for TLS/HTTPS support
              glib-networking

              # Image libraries
              libjpeg
              libtiff
              giflib
              libpng
              librsvg
              libwebp

              # PDF rendering (for pdf-tools epdfinfo server)
              poppler

              # Other useful libraries
              dbus
              sqlite
              tree-sitter

              # GMP for bignum support
              gmp
            ]
            # Linux-only dependencies
            ++ lib.optionals isLinux (with pkgs; [
              gst_all_1.gst-vaapi

              # VA-API for hardware video decoding (used by gst-va plugin)
              libva

              libselinux

              # For native compilation
              libgccjit

              # EGL/GPU/Vulkan for WPE and wgpu
              libGL
              vulkan-loader
              libxkbcommon
              mesa
              libdrm
              libgbm

              # Wayland
              wayland
              wayland-protocols

              # WPE WebKit
              wpewebkit
              libwpe
              libwpe-fdo

              # Weston for WPE backend
              weston

              # xdg-dbus-proxy for WebKit sandbox
              xdg-dbus-proxy
              gcc
            ]);

            # pkg-config paths for dev headers
            PKG_CONFIG_PATH = pkgs.lib.makeSearchPath "lib/pkgconfig" (with pkgs; [
              glib.dev
              cairo.dev
              gst_all_1.gstreamer.dev
              gst_all_1.gst-plugins-base.dev
              fontconfig.dev
              freetype.dev
              harfbuzz.dev
              libxml2.dev
              gnutls.dev
              zlib.dev
              ncurses.dev
              dbus.dev
              sqlite.dev
              tree-sitter
              gmp.dev
              libsoup_3.dev
              poppler.dev
            ]
            ++ lib.optionals isLinux [
              libva
              libselinux.dev
              libGL.dev
              libxkbcommon.dev
              libdrm.dev
              mesa
              wayland.dev
              wpewebkit
              libwpe
              libwpe-fdo
            ]);

            # For bindgen to find libclang
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

            shellHook = ''
              echo "=== Neomacs Development Environment ==="
              echo ""
              echo "Rust: $(rustc --version)"
              echo "Cargo: $(cargo --version)"
              echo "GStreamer: $(pkg-config --modversion gstreamer-1.0 2>/dev/null || echo 'not found')"
            ''
            # Linux-specific shell hook
            + lib.optionalString isLinux ''
              echo "xkbcommon: $(pkg-config --modversion xkbcommon 2>/dev/null || echo 'not found')"
              echo "WPE WebKit: $(pkg-config --modversion wpe-webkit-2.0 2>/dev/null || echo 'not found')"
              echo ""

              # Library path for runtime — DO NOT include ncurses here,
              # it causes glibc version contamination with system shell.
              # The linker adds RPATH for ncurses during compilation.
              export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath (with pkgs; [
                glib
                cairo
                gst_all_1.gstreamer
                gst_all_1.gst-plugins-base
                fontconfig
                freetype
                harfbuzz
                libxml2
                gnutls
                libjpeg
                libtiff
                giflib
                libpng
                librsvg
                libwebp
                dbus
                sqlite
                gmp
                libgccjit
                libsoup_3
                libGL
                vulkan-loader
                mesa
                libdrm
                libxkbcommon
                libgbm
                # X11 libs dynamically loaded by winit
                libx11
                libxcursor
                libxrandr
                libxi
                libxinerama
                wpewebkit
                libwpe
                libwpe-fdo
              ])}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

              # Vulkan ICD discovery — tell the Vulkan loader where Mesa's
              # driver JSON files are (e.g. intel_icd.x86_64.json for anv).
              # Without this, wgpu can't find Vulkan drivers and falls back to OpenGL.
              export VK_DRIVER_FILES="$(echo ${pkgs.mesa}/share/vulkan/icd.d/*.json | tr ' ' ':')"

              # WPE WebKit environment
              export WPE_BACKEND_LIBRARY="${pkgs.libwpe-fdo}/lib/libWPEBackend-fdo-1.0.so"
              export GIO_MODULE_DIR="${pkgs.glib-networking}/lib/gio/modules"
              export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
              export WEBKIT_USE_SINGLE_WEB_PROCESS=1
              export PATH="${pkgs.wpewebkit}/libexec/wpe-webkit-2.0:$PATH"

              # X11/Wayland display — preserve from parent env or detect from running session.
              # nix develop sanitizes env, so DISPLAY/XAUTHORITY may be lost.
              # Detect them from a running desktop session via /proc/<pid>/environ.
              _detect_display_env() {
                local _pid
                # NixOS wraps binaries, so process names may be e.g. ".kwin_x11-wrapp";
                # use substring match (no -x flag) to handle this.
                _pid=$(pgrep -u "$USER" kwin_x11 2>/dev/null | head -1)
                [ -z "$_pid" ] && _pid=$(pgrep -u "$USER" gnome-shell 2>/dev/null | head -1)
                [ -z "$_pid" ] && _pid=$(pgrep -u "$USER" Xorg 2>/dev/null | head -1)
                [ -z "$_pid" ] && _pid=$(pgrep -u "$USER" sway 2>/dev/null | head -1)
                [ -z "$_pid" ] && _pid=$(pgrep -u "$USER" Hyprland 2>/dev/null | head -1)
                if [ -n "$_pid" ] && [ -r "/proc/$_pid/environ" ]; then
                  if [ -z "$DISPLAY" ]; then
                    DISPLAY=$(tr '\0' '\n' < /proc/$_pid/environ | grep '^DISPLAY=' | head -1 | cut -d= -f2-)
                    [ -n "$DISPLAY" ] && export DISPLAY
                  fi
                  if [ -z "$XAUTHORITY" ] && [ -n "$DISPLAY" ]; then
                    XAUTHORITY=$(tr '\0' '\n' < /proc/$_pid/environ | grep '^XAUTHORITY=' | head -1 | cut -d= -f2-)
                    if [ -n "$XAUTHORITY" ] && [ -f "$XAUTHORITY" ]; then
                      export XAUTHORITY
                    elif [ -f "$HOME/.Xauthority" ]; then
                      export XAUTHORITY="$HOME/.Xauthority"
                    fi
                  fi
                  if [ -z "$WAYLAND_DISPLAY" ]; then
                    WAYLAND_DISPLAY=$(tr '\0' '\n' < /proc/$_pid/environ | grep '^WAYLAND_DISPLAY=' | head -1 | cut -d= -f2-)
                    [ -n "$WAYLAND_DISPLAY" ] && export WAYLAND_DISPLAY
                  fi
                fi
              }
              _detect_display_env
              unset -f _detect_display_env

              if [ -n "$DISPLAY" ]; then
                echo "Display: DISPLAY=$DISPLAY  XAUTHORITY=''${XAUTHORITY:-(unset)}"
                if ! timeout 2s ${pkgs.xdpyinfo}/bin/xdpyinfo >/dev/null 2>&1; then
                  export NEOMACS_X11_UNUSABLE=1
                  echo "Warning: X11 display handshake failed for DISPLAY=$DISPLAY."
                  echo "         GUI clients like winit/Neomacs may hang before the first window appears."
                  echo "         Run from a working desktop terminal, set a valid DISPLAY/XAUTHORITY,"
                  echo "         or use a private X server like Xvfb for automated probes."
                fi
              else
                echo "Display: (no X11/Wayland display detected)"
              fi
            ''
            # Darwin-specific shell hook
            + lib.optionalString isDarwin ''
              echo ""
              echo "Note: WPE WebKit is not available on macOS."
              echo "      WebKit-based features will be disabled."
            ''
            # Common shell hook (both platforms)
            + ''
              # Set default log levels (can be overridden before entering nix develop)
              export RUST_LOG="''${RUST_LOG:-info}"
              export NEOMACS_LOG="''${NEOMACS_LOG:-info}"

              echo ""
              echo "Build commands:"
              echo "  1. cargo build --release -p neomacs-bin"
              echo "  2. ./target/release/neomacs"
              echo ""
              echo "Logging (set before entering nix develop to override):"
              echo "  RUST_LOG=$RUST_LOG      (Rust render thread: trace|debug|info|warn|error)"
              echo "  NEOMACS_LOG=$NEOMACS_LOG  (Neomacs runtime: trace|debug|info|warn|error)"
              echo ""
            '';
          };
        }
      );

      # Legacy nix package removed with the deleted Emacs C build path.
      packages = forAllSystems (_system: { });
    };
}
