;;; neomacs-face-test.el --- Comprehensive face attribute rendering test -*- lexical-binding: t -*-

;; Test ALL Emacs face attributes to verify neomacs GPU renderer handles them.
;; Usage: emacs -Q -l test/neomacs/neomacs-face-test.el

;;; Commentary:

;; This test creates a buffer showing every face attribute Emacs supports,
;; organized by category.  Each attribute has a labeled sample so you can
;; visually verify the renderer draws it correctly.
;;
;; Known gaps in neomacs rendering (as of 2026-02-07):
;;   - Stipple: completely absent from pipeline
;;   - Distant-foreground: not checked in neomacsterm.c
;;   - Font width (condensed/expanded): not handled
;;
;; Implemented decorations:
;;   - Underline: all 5 styles (single, wave, double, dotted, dashed) + custom color
;;   - Strike-through: with optional custom color
;;   - Overline: with optional custom color
;;   - Box: border rects using box_color and box_line_width

;;; Code:

;; ============================================================================
;; Define test faces
;; ============================================================================

;; --- Foreground / Background colors ---
(defface neomacs-test-fg-red
  '((t :foreground "red"))
  "Red foreground.")

(defface neomacs-test-fg-green
  '((t :foreground "#00cc00"))
  "Green foreground (hex).")

(defface neomacs-test-fg-blue
  '((t :foreground "dodger blue"))
  "Blue foreground (named).")

(defface neomacs-test-bg-yellow
  '((t :background "yellow" :foreground "black"))
  "Yellow background.")

(defface neomacs-test-bg-dark
  '((t :background "#333333" :foreground "white"))
  "Dark background.")

(defface neomacs-test-fg-bg-combo
  '((t :foreground "white" :background "dark green"))
  "White on dark green.")

;; --- Font weight ---
(defface neomacs-test-bold
  '((t :weight bold))
  "Bold weight.")

(defface neomacs-test-light
  '((t :weight light))
  "Light weight.")

(defface neomacs-test-semi-bold
  '((t :weight semi-bold))
  "Semi-bold weight.")

(defface neomacs-test-extra-bold
  '((t :weight extra-bold))
  "Extra-bold weight.")

(defface neomacs-test-thin
  '((t :weight thin))
  "Thin weight.")

;; --- Font slant ---
(defface neomacs-test-italic
  '((t :slant italic))
  "Italic slant.")

(defface neomacs-test-oblique
  '((t :slant oblique))
  "Oblique slant.")

(defface neomacs-test-bold-italic
  '((t :weight bold :slant italic))
  "Bold + italic combined.")

;; --- Font size ---
(defface neomacs-test-small
  '((t :height 0.8))
  "Small text (80%).")

(defface neomacs-test-large
  '((t :height 1.5))
  "Large text (150%).")

(defface neomacs-test-huge
  '((t :height 2.0))
  "Huge text (200%).")

(defface neomacs-test-very-huge
  '((t :height 3.0))
  "Very huge text (300%).")

;; --- Font family ---
(defface neomacs-test-serif
  '((t :family "Serif"))
  "Serif font.")

(defface neomacs-test-sans
  '((t :family "Sans"))
  "Sans-serif font.")

(defface neomacs-test-mono
  '((t :family "Monospace"))
  "Monospace font (explicit).")

;; --- Underline styles ---
(defface neomacs-test-underline-simple
  '((t :underline t))
  "Simple underline.")

(defface neomacs-test-underline-color
  '((t :underline (:color "red" :style line)))
  "Red underline.")

(defface neomacs-test-underline-wave
  '((t :underline (:color "red" :style wave)))
  "Red wavy underline (like spell-check).")

(defface neomacs-test-underline-double
  '((t :underline (:color "blue" :style double-line)))
  "Blue double underline.")

(defface neomacs-test-underline-dotted
  '((t :underline (:color "green" :style dots)))
  "Green dotted underline.")

(defface neomacs-test-underline-dashed
  '((t :underline (:color "orange" :style dashes)))
  "Orange dashed underline.")

;; --- Overline ---
(defface neomacs-test-overline
  '((t :overline t))
  "Overline with foreground color.")

(defface neomacs-test-overline-color
  '((t :overline "red"))
  "Red overline.")

;; --- Strike-through ---
(defface neomacs-test-strike
  '((t :strike-through t))
  "Strike-through with foreground color.")

(defface neomacs-test-strike-color
  '((t :strike-through "red"))
  "Red strike-through.")

;; --- Box ---
(defface neomacs-test-box-simple
  '((t :box t))
  "Simple box (1px foreground).")

(defface neomacs-test-box-color
  '((t :box (:line-width 2 :color "red")))
  "Red box, 2px wide.")

(defface neomacs-test-box-raised
  '((t :box (:line-width 2 :color "gray70" :style released-button)))
  "3D raised button box.")

(defface neomacs-test-box-pressed
  '((t :box (:line-width 2 :color "gray70" :style pressed-button)))
  "3D pressed button box.")

(defface neomacs-test-box-negative
  '((t :box (:line-width -2 :color "blue")))
  "Box with negative width (drawn inside).")

;; --- Inverse video ---
(defface neomacs-test-inverse
  '((t :inverse-video t))
  "Inverse video.")

(defface neomacs-test-inverse-colored
  '((t :foreground "cyan" :background "dark red" :inverse-video t))
  "Inverse video with custom colors.")

;; --- Stipple ---
(defface neomacs-test-stipple
  '((t :stipple "cross_weave" :background "light yellow"))
  "Stipple pattern (X11 bitmap).")

;; --- Distant foreground ---
(defface neomacs-test-distant-fg
  '((t :foreground "white" :distant-foreground "black" :background "white"))
  "Distant foreground (should show black since bg=white=fg).")

;; --- Font width ---
(defface neomacs-test-condensed
  '((t :width condensed))
  "Condensed width.")

(defface neomacs-test-expanded
  '((t :width expanded))
  "Expanded width.")

;; --- Combinations ---
(defface neomacs-test-all-decorations
  '((t :weight bold :slant italic :underline (:color "red" :style wave)
       :overline "blue" :strike-through "green"
       :box (:line-width 1 :color "orange")
       :foreground "white" :background "gray20"))
  "All decorations combined.")

(defface neomacs-test-mode-line-like
  '((t :box (:line-width (1 . -1) :style released-button)
       :background "gray75" :foreground "black"
       :weight bold))
  "Mode-line-like face.")

(defface neomacs-test-link-like
  '((t :foreground "dodger blue" :underline t))
  "Hyperlink-like face.")

(defface neomacs-test-error-like
  '((t :foreground "red" :weight bold :underline (:color "red" :style wave)))
  "Error/warning-like face.")

(defface neomacs-test-diff-added
  '((t :foreground "dark green" :background "#ddffdd"))
  "Diff added line face.")

(defface neomacs-test-diff-removed
  '((t :foreground "dark red" :background "#ffdddd" :strike-through t))
  "Diff removed line face.")

;; --- Extend ---
(defface neomacs-test-extend
  '((t :background "light blue" :extend t))
  "Face with :extend t (should color past EOL).")

(defface neomacs-test-no-extend
  '((t :background "light green" :extend nil))
  "Face with :extend nil (should NOT color past EOL).")

;; --- Inherit ---
(defface neomacs-test-inherit-bold
  '((t :inherit bold))
  "Inherits from bold face.")

(defface neomacs-test-inherit-multi
  '((t :inherit (bold italic)))
  "Inherits from bold and italic faces.")

;; --- Font family / height / weight matrix ---
(defvar neomacs-face-test-font-combo-families
  '("JetBrains Mono" "Hack" "DejaVu Sans Mono" "Noto Sans Mono")
  "Font families used in family/height/weight combination tests.")

(defvar neomacs-face-test-font-combo-heights
  '(0.9 1.0 1.2 1.6)
  "Relative font heights used in family/height/weight combination tests.")

(defvar neomacs-face-test-font-combo-weights
  '(normal semi-bold bold extra-bold)
  "Font weights used in family/height/weight combination tests.")

;; ============================================================================
;; Build the test buffer
;; ============================================================================

(defun neomacs-face-test--insert-section (title)
  "Insert a section TITLE header."
  (insert "\n")
  (let ((start (point)))
    (insert (format "=== %s ===\n" title))
    (put-text-property start (point) 'face '(:weight bold :height 1.2 :foreground "gold"))))

(defun neomacs-face-test--insert-sample (label face-name)
  "Insert a sample line with LABEL using FACE-NAME."
  (insert (format "  %-35s " label))
  (let ((start (point)))
    (insert "The quick brown fox jumps over the lazy dog 0123456789")
    (put-text-property start (point) 'face face-name))
  (insert "\n"))

(defun neomacs-face-test--insert-sample-text (label face-name text)
  "Insert a sample line with LABEL using FACE-NAME over TEXT."
  (insert (format "  %-35s " label))
  (let ((start (point)))
    (insert text)
    (put-text-property start (point) 'face face-name))
  (insert "\n"))

(defun neomacs-face-test--insert-inline-sample (label face-name)
  "Insert sample with face applied to middle of line."
  (insert (format "  %-35s normal " label))
  (let ((start (point)))
    (insert "FACED TEXT")
    (put-text-property start (point) 'face face-name))
  (insert " normal\n"))

(defun neomacs-face-test ()
  "Create comprehensive face attribute test buffer."
  (interactive)
  (let ((buf (get-buffer-create "*Neomacs Face Test*")))
    (switch-to-buffer buf)
    (erase-buffer)

    ;; Header
    (let ((start (point)))
      (insert "NEOMACS FACE ATTRIBUTE RENDERING TEST\n")
      (put-text-property start (point) 'face '(:weight bold :height 1.8 :foreground "cyan")))
    (insert "Check each line visually. Broken rendering = missing face attribute support.\n")
    (insert (format "Emacs version: %s\n" emacs-version))
    (insert (format "Window system: %s\n" window-system))
    (insert (make-string 78 ?-) "\n")

    ;; === FOREGROUND / BACKGROUND ===
    (neomacs-face-test--insert-section "FOREGROUND / BACKGROUND COLORS")
    (neomacs-face-test--insert-sample "Red foreground:" 'neomacs-test-fg-red)
    (neomacs-face-test--insert-sample "Green foreground (hex):" 'neomacs-test-fg-green)
    (neomacs-face-test--insert-sample "Blue foreground (named):" 'neomacs-test-fg-blue)
    (neomacs-face-test--insert-sample "Yellow background:" 'neomacs-test-bg-yellow)
    (neomacs-face-test--insert-sample "Dark background:" 'neomacs-test-bg-dark)
    (neomacs-face-test--insert-sample "White on dark green:" 'neomacs-test-fg-bg-combo)

    ;; === FONT WEIGHT ===
    (neomacs-face-test--insert-section "FONT WEIGHT")
    (neomacs-face-test--insert-sample "Thin:" 'neomacs-test-thin)
    (neomacs-face-test--insert-sample "Light:" 'neomacs-test-light)
    (insert (format "  %-35s " "Normal (default):"))
    (insert "The quick brown fox jumps over the lazy dog 0123456789\n")
    (neomacs-face-test--insert-sample "Semi-bold:" 'neomacs-test-semi-bold)
    (neomacs-face-test--insert-sample "Bold:" 'neomacs-test-bold)
    (neomacs-face-test--insert-sample "Extra-bold:" 'neomacs-test-extra-bold)

    ;; === FONT SLANT ===
    (neomacs-face-test--insert-section "FONT SLANT")
    (neomacs-face-test--insert-sample "Italic:" 'neomacs-test-italic)
    (neomacs-face-test--insert-sample "Oblique:" 'neomacs-test-oblique)
    (neomacs-face-test--insert-sample "Bold + Italic:" 'neomacs-test-bold-italic)

    ;; === FONT SIZE ===
    (neomacs-face-test--insert-section "FONT SIZE")
    (neomacs-face-test--insert-sample "Small (80%):" 'neomacs-test-small)
    (insert (format "  %-35s " "Normal (100%):"))
    (insert "The quick brown fox jumps over the lazy dog\n")
    (neomacs-face-test--insert-sample "Large (150%):" 'neomacs-test-large)
    (neomacs-face-test--insert-sample "Huge (200%):" 'neomacs-test-huge)
    (neomacs-face-test--insert-sample "Very Huge (300%):" 'neomacs-test-very-huge)

    ;; === FONT FAMILY ===
    (neomacs-face-test--insert-section "FONT FAMILY")
    (neomacs-face-test--insert-sample "Serif:" 'neomacs-test-serif)
    (neomacs-face-test--insert-sample "Sans-serif:" 'neomacs-test-sans)
    (neomacs-face-test--insert-sample "Monospace (explicit):" 'neomacs-test-mono)

    ;; === FONT FAMILY / HEIGHT / WEIGHT COMBINATIONS ===
    (neomacs-face-test--insert-section "FONT FAMILY x HEIGHT x WEIGHT (ASCII + CJK)")
    (insert "  Matrix rows exercise mixed family/height/weight combinations.\n")
    (insert "  Verify spacing/alignment for both ASCII and CJK text across transitions.\n")
    (insert "  Sample text: a好好b  ABCXYZ 0123456789  -> <= >=\n\n")
    (dolist (family neomacs-face-test-font-combo-families)
      (let ((start (point)))
        (insert (format "  -- family: %s --\n" family))
        (put-text-property start (point) 'face '(:weight bold :foreground "light steel blue")))
      (dolist (height neomacs-face-test-font-combo-heights)
        (dolist (weight neomacs-face-test-font-combo-weights)
          (neomacs-face-test--insert-sample-text
           (format "h=%s w=%s:" height weight)
           `(:family ,family :height ,height :weight ,weight)
           "a好好b  ABCXYZ 0123456789  -> <= >=")))
      (insert "\n"))

    (insert "  Adjacent transitions (no gaps expected): ")
    (let ((s (point)))
      (insert "A好")
      (put-text-property s (point) 'face
                         '(:family "JetBrains Mono" :height 1.0 :weight normal)))
    (let ((s (point)))
      (insert "B好")
      (put-text-property s (point) 'face
                         '(:family "Hack" :height 1.2 :weight semi-bold)))
    (let ((s (point)))
      (insert "C好")
      (put-text-property s (point) 'face
                         '(:family "DejaVu Sans Mono" :height 1.6 :weight bold)))
    (let ((s (point)))
      (insert "D好")
      (put-text-property s (point) 'face
                         '(:family "Noto Sans Mono" :height 0.9 :weight extra-bold)))
    (insert "\n")

    ;; === FONT WIDTH ===
    (neomacs-face-test--insert-section "FONT WIDTH (condensed/expanded)")
    (neomacs-face-test--insert-sample "Condensed:" 'neomacs-test-condensed)
    (neomacs-face-test--insert-sample "Expanded:" 'neomacs-test-expanded)

    ;; === UNDERLINE ===
    (neomacs-face-test--insert-section "UNDERLINE (5 styles)")
    (neomacs-face-test--insert-sample "Simple underline (t):" 'neomacs-test-underline-simple)
    (neomacs-face-test--insert-sample "Red line underline:" 'neomacs-test-underline-color)
    (neomacs-face-test--insert-sample "Red wave underline:" 'neomacs-test-underline-wave)
    (neomacs-face-test--insert-sample "Blue double underline:" 'neomacs-test-underline-double)
    (neomacs-face-test--insert-sample "Green dotted underline:" 'neomacs-test-underline-dotted)
    (neomacs-face-test--insert-sample "Orange dashed underline:" 'neomacs-test-underline-dashed)

    ;; === OVERLINE ===
    (neomacs-face-test--insert-section "OVERLINE")
    (neomacs-face-test--insert-sample "Overline (t):" 'neomacs-test-overline)
    (neomacs-face-test--insert-sample "Red overline:" 'neomacs-test-overline-color)

    ;; === STRIKE-THROUGH ===
    (neomacs-face-test--insert-section "STRIKE-THROUGH")
    (neomacs-face-test--insert-sample "Strike-through (t):" 'neomacs-test-strike)
    (neomacs-face-test--insert-sample "Red strike-through:" 'neomacs-test-strike-color)

    ;; === BOX ===
    (neomacs-face-test--insert-section "BOX (border around text)")
    (neomacs-face-test--insert-inline-sample "Simple box (1px):" 'neomacs-test-box-simple)
    (neomacs-face-test--insert-inline-sample "Red box (2px):" 'neomacs-test-box-color)
    (neomacs-face-test--insert-inline-sample "3D raised button:" 'neomacs-test-box-raised)
    (neomacs-face-test--insert-inline-sample "3D pressed button:" 'neomacs-test-box-pressed)
    (neomacs-face-test--insert-inline-sample "Negative width (inside):" 'neomacs-test-box-negative)

    ;; === INVERSE VIDEO ===
    (neomacs-face-test--insert-section "INVERSE VIDEO")
    (neomacs-face-test--insert-sample "Inverse video:" 'neomacs-test-inverse)
    (neomacs-face-test--insert-sample "Inverse + custom colors:" 'neomacs-test-inverse-colored)

    ;; === STIPPLE ===
    (neomacs-face-test--insert-section "STIPPLE (X11 bitmap pattern)")
    (neomacs-face-test--insert-sample "Stipple pattern:" 'neomacs-test-stipple)

    ;; === DISTANT FOREGROUND ===
    (neomacs-face-test--insert-section "DISTANT FOREGROUND")
    (neomacs-face-test--insert-sample "Distant-fg (white bg+fg):" 'neomacs-test-distant-fg)

    ;; === EXTEND ===
    (neomacs-face-test--insert-section "EXTEND (background past EOL)")
    (let ((start (point)))
      (insert "  extend t:                          short text\n")
      (put-text-property start (1- (point)) 'face 'neomacs-test-extend))
    (let ((start (point)))
      (insert "  extend nil:                        short text\n")
      (put-text-property start (1- (point)) 'face 'neomacs-test-no-extend))

    ;; === INHERIT ===
    (neomacs-face-test--insert-section "INHERIT")
    (neomacs-face-test--insert-sample "Inherit bold:" 'neomacs-test-inherit-bold)
    (neomacs-face-test--insert-sample "Inherit bold+italic:" 'neomacs-test-inherit-multi)

    ;; === COMBINATIONS ===
    (neomacs-face-test--insert-section "COMBINATIONS (multiple attributes)")
    (neomacs-face-test--insert-sample "All decorations:" 'neomacs-test-all-decorations)
    (neomacs-face-test--insert-inline-sample "Mode-line-like:" 'neomacs-test-mode-line-like)
    (neomacs-face-test--insert-sample "Link-like:" 'neomacs-test-link-like)
    (neomacs-face-test--insert-sample "Error-like:" 'neomacs-test-error-like)
    (neomacs-face-test--insert-sample "Diff added:" 'neomacs-test-diff-added)
    (neomacs-face-test--insert-sample "Diff removed:" 'neomacs-test-diff-removed)

    ;; === BUILT-IN FACES ===
    (neomacs-face-test--insert-section "BUILT-IN EMACS FACES")
    (neomacs-face-test--insert-sample "font-lock-keyword-face:" 'font-lock-keyword-face)
    (neomacs-face-test--insert-sample "font-lock-string-face:" 'font-lock-string-face)
    (neomacs-face-test--insert-sample "font-lock-comment-face:" 'font-lock-comment-face)
    (neomacs-face-test--insert-sample "font-lock-function-name-face:" 'font-lock-function-name-face)
    (neomacs-face-test--insert-sample "font-lock-variable-name-face:" 'font-lock-variable-name-face)
    (neomacs-face-test--insert-sample "font-lock-type-face:" 'font-lock-type-face)
    (neomacs-face-test--insert-sample "font-lock-constant-face:" 'font-lock-constant-face)
    (neomacs-face-test--insert-sample "font-lock-warning-face:" 'font-lock-warning-face)
    (neomacs-face-test--insert-sample "error:" 'error)
    (neomacs-face-test--insert-sample "warning:" 'warning)
    (neomacs-face-test--insert-sample "success:" 'success)
    (neomacs-face-test--insert-sample "highlight:" 'highlight)
    (neomacs-face-test--insert-sample "region:" 'region)
    (neomacs-face-test--insert-sample "isearch:" 'isearch)
    (neomacs-face-test--insert-sample "lazy-highlight:" 'lazy-highlight)
    (neomacs-face-test--insert-sample "minibuffer-prompt:" 'minibuffer-prompt)
    (neomacs-face-test--insert-sample "link:" 'link)
    (neomacs-face-test--insert-sample "link-visited:" 'link-visited)
    (neomacs-face-test--insert-sample "shadow:" 'shadow)
    (neomacs-face-test--insert-sample "trailing-whitespace:" 'trailing-whitespace)
    (neomacs-face-test--insert-sample "escape-glyph:" 'escape-glyph)
    (neomacs-face-test--insert-sample "homoglyph:" 'homoglyph)

    ;; === MIXED INLINE TEST ===
    (neomacs-face-test--insert-section "MIXED INLINE (face changes within a line)")
    (insert "  ")
    (let ((s (point))) (insert "normal") (put-text-property s (point) 'face 'default))
    (insert " ")
    (let ((s (point))) (insert "BOLD") (put-text-property s (point) 'face 'bold))
    (insert " ")
    (let ((s (point))) (insert "italic") (put-text-property s (point) 'face 'italic))
    (insert " ")
    (let ((s (point))) (insert "BOLD-ITALIC") (put-text-property s (point) 'face 'bold-italic))
    (insert " ")
    (let ((s (point))) (insert "underline") (put-text-property s (point) 'face 'underline))
    (insert " ")
    (let ((s (point))) (insert "RED") (put-text-property s (point) 'face '(:foreground "red")))
    (insert " ")
    (let ((s (point))) (insert "BG-YELLOW") (put-text-property s (point) 'face '(:background "yellow" :foreground "black")))
    (insert " ")
    (let ((s (point))) (insert "inverse") (put-text-property s (point) 'face '(:inverse-video t)))
    (insert " normal\n")

    ;; === ADJACENT DIFFERENT FACES ===
    (neomacs-face-test--insert-section "ADJACENT FACES (no gaps between face transitions)")
    (insert "  ")
    (let ((s (point))) (insert "RED") (put-text-property s (point) 'face '(:foreground "red")))
    (let ((s (point))) (insert "GREEN") (put-text-property s (point) 'face '(:foreground "green")))
    (let ((s (point))) (insert "BLUE") (put-text-property s (point) 'face '(:foreground "blue")))
    (let ((s (point))) (insert "YELLOW") (put-text-property s (point) 'face '(:foreground "yellow")))
    (let ((s (point))) (insert "CYAN") (put-text-property s (point) 'face '(:foreground "cyan")))
    (let ((s (point))) (insert "MAGENTA") (put-text-property s (point) 'face '(:foreground "magenta")))
    (insert "\n")
    (insert "  ")
    (let ((s (point))) (insert " RED BG ") (put-text-property s (point) 'face '(:background "red" :foreground "white")))
    (let ((s (point))) (insert " GREEN BG ") (put-text-property s (point) 'face '(:background "green" :foreground "white")))
    (let ((s (point))) (insert " BLUE BG ") (put-text-property s (point) 'face '(:background "blue" :foreground "white")))
    (let ((s (point))) (insert " DARK BG ") (put-text-property s (point) 'face '(:background "#333" :foreground "white")))
    (insert "\n")

    ;; === OVERLAPPING OVERLAYS ===
    (neomacs-face-test--insert-section "OVERLAYS (overlay face priority)")
    (let ((line-start (point)))
      (insert "  This text has overlapping overlays: ")
      (let* ((s (point))
             (e (progn (insert "OVERLAY-TEXT") (point)))
             (ov1 (make-overlay s e))
             (ov2 (make-overlay (+ s 3) (- e 3))))
        (overlay-put ov1 'face '(:background "light blue"))
        (overlay-put ov2 'face '(:background "light green" :weight bold)))
      (insert "\n"))

    ;; === SUMMARY ===
    (neomacs-face-test--insert-section "EXPECTED RENDERING STATUS")
    (insert "
  SHOULD WORK (basic rendering):
    - Foreground/background colors
    - Bold, italic, bold-italic
    - Font size (relative heights)
    - Font family changes
    - Region/highlight/isearch faces

  SHOULD WORK (text decorations):
    - Underline (all 5 styles: single, wave, double, dotted, dashed)
    - Underline custom color
    - Strike-through (with optional custom color)
    - Overline (with optional custom color)
    - Box borders (line style, with custom color and width)

  MAY BE BROKEN (needs verification):
    - Inverse video on text (works for cursor, unclear for faces)
    - Font weight granularity (thin/light/semi-bold/extra-bold)
    - Relative font sizes within a line
    - Box 3D styles (raised/pressed button)

  LIKELY BROKEN (not implemented in Rust renderer):
    - Stipple -- completely absent from pipeline
    - Distant-foreground -- not checked in neomacsterm.c
    - Font width (condensed/expanded)
    - :extend attribute (background past EOL)
")

    (goto-char (point-min))
    (setq buffer-read-only t)
    (message "Face test buffer ready. Scroll through to check each attribute.")))

;; Run automatically when loaded
(neomacs-face-test)

;;; neomacs-face-test.el ends here
