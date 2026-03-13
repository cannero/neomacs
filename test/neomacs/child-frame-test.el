;;; child-frame-test.el --- Test child frame rendering -*- lexical-binding: t -*-

;; Test child frame GPU overlay compositing: creation, positioning,
;; z-order, mouse routing, cursor animation, styling, and cleanup.
;; Usage: emacs -Q -l test/neomacs/child-frame-test.el

;;; Code:

(defvar child-frame-test--frames nil
  "List of child frames created during test.")

(defvar child-frame-test--step 0
  "Current test step for the automated sequence.")

(defun child-frame-test--cleanup ()
  "Delete all test child frames and force a redisplay."
  (dolist (f child-frame-test--frames)
    (when (frame-live-p f)
      (delete-frame f)))
  (setq child-frame-test--frames nil)
  (redisplay t))

(defun child-frame-test--make-child (name x y w h &optional params)
  "Create a child frame at (X, Y) with size (W, H) and extra PARAMS.
Insert some text into its buffer and return the frame."
  (let* ((buf (get-buffer-create (format "*child-%s*" name)))
         (frame (make-frame
                 (append
                  `((parent-frame . ,(selected-frame))
                    (left . ,x) (top . ,y)
                    (width . ,w) (height . ,h)
                    (minibuffer . nil)
                    (no-accept-focus . nil)
                    (child-frame-border-width . 2)
                    (internal-border-width . 4)
                    (undecorated . t)
                    (visibility . t))
                  params))))
    (with-selected-frame frame
      (switch-to-buffer buf)
      (erase-buffer))
    (push frame child-frame-test--frames)
    frame))

;; ============================================================================
;; Test 1: Basic child frame creation and positioning
;; ============================================================================

(defun child-frame-test--basic ()
  "Create child frames at various positions."
  (child-frame-test--cleanup)
  (let ((f1 (child-frame-test--make-child "top-left" 20 20 35 8))
        (f2 (child-frame-test--make-child "center" 200 150 40 10))
        (f3 (child-frame-test--make-child "bottom-right" 400 300 30 6)))
    (with-selected-frame f1
      (insert (propertize "Child Frame: Top-Left\n" 'face '(:foreground "cyan" :weight bold)))
      (insert "Position: (20, 20)\n")
      (insert "This frame should appear near\nthe top-left corner.\n")
      (dotimes (i 4)
        (insert (format "  line %d\n" (1+ i)))))
    (with-selected-frame f2
      (insert (propertize "Child Frame: Center\n" 'face '(:foreground "gold" :weight bold)))
      (insert "Position: (200, 150)\n")
      (insert "This is the center frame.\n")
      (insert "It should have rounded corners\n")
      (insert "and a drop shadow.\n")
      (dotimes (i 5)
        (insert (format "  content line %d\n" (1+ i)))))
    (with-selected-frame f3
      (insert (propertize "Child Frame: Bottom-Right\n" 'face '(:foreground "lime green" :weight bold)))
      (insert "Position: (400, 300)\n")
      (insert "Smaller frame.\n")
      (dotimes (i 3)
        (insert (format "  line %d\n" (1+ i))))))
  (message "Test 1: Basic child frames created (3 frames at different positions)"))

;; ============================================================================
;; Test 2: Z-order / stacking
;; ============================================================================

(defun child-frame-test--z-order ()
  "Create overlapping child frames to test z-order."
  (child-frame-test--cleanup)
  (let ((f1 (child-frame-test--make-child "back" 100 100 35 8))
        (f2 (child-frame-test--make-child "middle" 130 130 35 8))
        (f3 (child-frame-test--make-child "front" 160 160 35 8)))
    (with-selected-frame f1
      (face-remap-add-relative 'default :background "#442222")
      (insert (propertize "BACK (z=0)\n" 'face '(:foreground "salmon" :weight bold)))
      (insert "This should be behind.\n")
      (dotimes (i 6) (insert (format "  back line %d\n" (1+ i)))))
    (with-selected-frame f2
      (face-remap-add-relative 'default :background "#224422")
      (insert (propertize "MIDDLE (z=1)\n" 'face '(:foreground "light green" :weight bold)))
      (insert "This is the middle layer.\n")
      (dotimes (i 6) (insert (format "  mid line %d\n" (1+ i)))))
    (with-selected-frame f3
      (face-remap-add-relative 'default :background "#222244")
      (insert (propertize "FRONT (z=2)\n" 'face '(:foreground "light blue" :weight bold)))
      (insert "This should be on top.\n")
      (dotimes (i 6) (insert (format "  front line %d\n" (1+ i)))))
    ;; Raise to establish z-order
    (raise-frame f3)
    (raise-frame f2)
    (raise-frame f1)
    ;; Now raise in order: f1 first (lowest), f3 last (highest)
    (raise-frame f1)
    (raise-frame f2)
    (raise-frame f3))
  (message "Test 2: Z-order — 3 overlapping frames (back=red, mid=green, front=blue)"))

;; ============================================================================
;; Test 3: Alpha transparency
;; ============================================================================

(defun child-frame-test--alpha ()
  "Create child frames with different alpha transparency levels."
  (child-frame-test--cleanup)
  ;; First put some visible content in the parent
  (with-current-buffer (get-buffer-create "*child-frame-test*")
    (erase-buffer)
    (dotimes (i 40)
      (insert (format "Parent frame background line %03d — " (1+ i)))
      (insert (propertize "COLORED TEXT" 'face `(:foreground ,(nth (mod i 6) '("red" "green" "blue" "yellow" "cyan" "magenta")))))
      (insert " — visible through transparent child frames\n"))
    (goto-char (point-min)))
  (switch-to-buffer "*child-frame-test*")
  (let ((f1 (child-frame-test--make-child "opaque" 50 50 35 6
              '((alpha-background . 100))))
        (f2 (child-frame-test--make-child "semi" 200 50 35 6
              '((alpha-background . 70))))
        (f3 (child-frame-test--make-child "transparent" 350 50 35 6
              '((alpha-background . 40)))))
    (with-selected-frame f1
      (insert (propertize "100% Opaque\n" 'face '(:weight bold)))
      (insert "Fully opaque background.\n")
      (insert "Can't see parent text.\n")
      (dotimes (i 3) (insert (format "  line %d\n" (1+ i)))))
    (with-selected-frame f2
      (insert (propertize "70% Alpha\n" 'face '(:weight bold)))
      (insert "Semi-transparent.\n")
      (insert "Parent text bleeds through.\n")
      (dotimes (i 3) (insert (format "  line %d\n" (1+ i)))))
    (with-selected-frame f3
      (insert (propertize "40% Alpha\n" 'face '(:weight bold)))
      (insert "Very transparent.\n")
      (insert "Parent clearly visible.\n")
      (dotimes (i 3) (insert (format "  line %d\n" (1+ i))))))
  (message "Test 3: Alpha transparency — opaque(100%%), semi(70%%), transparent(40%%)"))

;; ============================================================================
;; Test 4: Border styling
;; ============================================================================

(defun child-frame-test--borders ()
  "Test different border widths and colors."
  (child-frame-test--cleanup)
  (let ((f1 (child-frame-test--make-child "thin-border" 50 50 30 6
              '((child-frame-border-width . 1))))
        (f2 (child-frame-test--make-child "thick-border" 250 50 30 6
              '((child-frame-border-width . 4))))
        (f3 (child-frame-test--make-child "no-border" 50 220 30 6
              '((child-frame-border-width . 0))))
        (f4 (child-frame-test--make-child "wide-internal" 250 220 30 6
              '((child-frame-border-width . 2) (internal-border-width . 12)))))
    (dolist (pair `((,f1 "1px border")
                    (,f2 "4px border")
                    (,f3 "No border")
                    (,f4 "2px border + 12px internal")))
      (with-selected-frame (car pair)
        (insert (propertize (format "%s\n" (cadr pair)) 'face '(:weight bold)))
        (insert "Check border rendering.\n")
        (dotimes (i 4) (insert (format "  line %d\n" (1+ i)))))))
  (message "Test 4: Border styles — thin(1px), thick(4px), none, wide internal"))

;; ============================================================================
;; Test 5: Child frame with rich face content
;; ============================================================================

(defun child-frame-test--rich-content ()
  "Child frame with various faces to verify face rendering in child frames."
  (child-frame-test--cleanup)
  (let ((f (child-frame-test--make-child "rich" 100 80 50 16)))
    (with-selected-frame f
      (insert (propertize "Rich Content Child Frame\n" 'face '(:height 1.3 :foreground "gold" :weight bold)))
      (insert (propertize "Bold text " 'face 'bold))
      (insert (propertize "Italic text " 'face 'italic))
      (insert (propertize "Bold-italic\n" 'face 'bold-italic))
      (insert (propertize "Underlined " 'face '(:underline t)))
      (insert (propertize "Wave underline " 'face '(:underline (:style wave :color "red"))))
      (insert (propertize "Strikethrough\n" 'face '(:strike-through t)))
      (insert (propertize "Red " 'face '(:foreground "red")))
      (insert (propertize "Green " 'face '(:foreground "green")))
      (insert (propertize "Blue " 'face '(:foreground "blue")))
      (insert (propertize "Yellow " 'face '(:foreground "yellow")))
      (insert (propertize "Cyan\n" 'face '(:foreground "cyan")))
      (insert (propertize " BG Red " 'face '(:background "dark red" :foreground "white")))
      (insert (propertize " BG Green " 'face '(:background "dark green" :foreground "white")))
      (insert (propertize " BG Blue \n" 'face '(:background "dark blue" :foreground "white")))
      (insert (propertize "Large text " 'face '(:height 1.5)))
      (insert (propertize "Small text\n" 'face '(:height 0.8)))
      (insert (propertize "Box face " 'face '(:box (:line-width 1 :color "orange"))))
      (insert (propertize " Overline\n" 'face '(:overline "red")))
      (insert "\nCursor should blink here.")
      (goto-char (point-max))))
  (message "Test 5: Rich content — faces, decorations, sizes inside child frame"))

;; ============================================================================
;; Test 6: Multiple child frames with cursor focus
;; ============================================================================

(defun child-frame-test--cursor-focus ()
  "Test cursor animation across parent and child frames."
  (child-frame-test--cleanup)
  (let ((f1 (child-frame-test--make-child "editable-1" 50 80 40 8
              '((no-accept-focus . nil))))
        (f2 (child-frame-test--make-child "editable-2" 350 80 40 8
              '((no-accept-focus . nil)))))
    (with-selected-frame f1
      (insert "Editable child frame 1\n")
      (insert "Click here to focus.\n")
      (insert "Cursor should animate\nwhen switching between\nparent and child frames.\n")
      (insert "Type here: "))
    (with-selected-frame f2
      (insert "Editable child frame 2\n")
      (insert "Another editable frame.\n")
      (insert "Focus should move cursor\nsmooth animation.\n")
      (insert "Type here: ")))
  (message "Test 6: Cursor focus — click child frames, cursor should animate"))

;; ============================================================================
;; Test 7: Child frame style configuration
;; ============================================================================

(defun child-frame-test--style-config ()
  "Test neomacs-set-child-frame-style with different params."
  (child-frame-test--cleanup)
  ;; Style: large corner radius, heavy shadow
  (when (fboundp 'neomacs-set-child-frame-style)
    (neomacs-set-child-frame-style
     :corner-radius 16 :shadow t
     :shadow-layers 8 :shadow-offset 3 :shadow-opacity 40))
  (let ((f (child-frame-test--make-child "styled" 150 100 40 10)))
    (with-selected-frame f
      (insert (propertize "Custom Styled Frame\n" 'face '(:foreground "orchid" :weight bold :height 1.2)))
      (insert "Corner radius: 16px\n")
      (insert "Shadow layers: 8\n")
      (insert "Shadow offset: 3px\n")
      (insert "Shadow opacity: 40%\n")
      (insert "\n")
      (insert "This frame should have\n")
      (insert "more rounded corners and\n")
      (insert "a heavier drop shadow.")))
  (message "Test 7: Custom style — corner-radius=16, shadow-layers=8, offset=3, opacity=40%%"))

;; ============================================================================
;; Test 8: Rapid create/delete (stale cleanup)
;; ============================================================================

(defun child-frame-test--rapid-lifecycle ()
  "Create and delete child frames rapidly to test stale cleanup."
  (child-frame-test--cleanup)
  (let ((created 0)
        (deleted 0))
    (dotimes (i 5)
      (let ((f (child-frame-test--make-child
                (format "rapid-%d" i)
                (+ 50 (* i 80)) (+ 50 (* i 40)) 25 5)))
        (with-selected-frame f
          (insert (format "Frame %d\n" (1+ i)))
          (insert "Created then deleted\n"))
        (setq created (1+ created))
        (redisplay t)
        ;; Delete every other frame immediately
        (when (= (mod i 2) 0)
          (delete-frame f)
          (setq child-frame-test--frames
                (delq f child-frame-test--frames))
          (setq deleted (1+ deleted)))))
    ;; Clean up all remaining frames too
    (child-frame-test--cleanup)
    (message "Test 8: Rapid lifecycle -- created %d, deleted %d, all cleaned up"
             created deleted)))

;; ============================================================================
;; Test 9: Posframe-like tooltip
;; ============================================================================

(defun child-frame-test--tooltip ()
  "Create a posframe-style tooltip near point."
  (child-frame-test--cleanup)
  ;; Reset style to default
  (when (fboundp 'neomacs-set-child-frame-style)
    (neomacs-set-child-frame-style
     :corner-radius 8 :shadow t
     :shadow-layers 4 :shadow-offset 2 :shadow-opacity 30))
  (let ((pos (window-absolute-pixel-position)))
    (when pos
      (let ((f (child-frame-test--make-child "tooltip"
                (+ (car pos) 10)
                (+ (cdr pos) 20)
                45 5
                '((no-accept-focus . t)))))
        (with-selected-frame f
          (face-remap-add-relative 'default :background "#ffffcc" :foreground "black")
          (insert (propertize " Tooltip " 'face '(:weight bold :foreground "dark orange")))
          (insert "\n")
          (insert " This is a posframe-style tooltip.\n")
          (insert " It appears near the cursor position.\n")
          (insert " no-accept-focus = t")))))
  (message "Test 9: Tooltip — posframe-like popup near cursor"))

;; ============================================================================
;; Test 10: Rounded corners clipping
;; ============================================================================

(defun child-frame-test--rounded-corners ()
  "Test rounded corner clipping -- content should not bleed past corners.
Creates 4 child frames simultaneously with corner radii 0, 8, 20, 30."
  (child-frame-test--cleanup)
  ;; Put visible content in parent so we can verify clipping
  (with-current-buffer (get-buffer-create "*child-frame-test*")
    (erase-buffer)
    (dotimes (i 40)
      (insert (format "Parent bg line %03d -- " (1+ i)))
      (insert (propertize "VISIBLE THROUGH CORNERS?"
                          'face '(:foreground "red")))
      (insert "\n"))
    (goto-char (point-min)))
  (switch-to-buffer "*child-frame-test*")
  ;; Helper to fill a child frame with colored content that would bleed
  (let ((fill-frame
         (lambda (f title bg-color fill-color heading-color)
           (with-selected-frame f
             (face-remap-add-relative 'default :background bg-color)
             (insert (propertize (concat title "\n")
                                'face (list :foreground heading-color :weight 'bold)))
             (insert (propertize "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX\n"
                                'face (list :background heading-color :foreground "white")))
             (dotimes (i 6)
               (insert (propertize
                        (format "  filled line %d                        \n" (1+ i))
                        'face (list :background fill-color))))))))
    ;; Frame 1: sharp corners (radius 0)
    (when (fboundp 'neomacs-set-child-frame-style)
      (neomacs-set-child-frame-style :corner-radius 0 :shadow nil))
    (let ((f1 (child-frame-test--make-child "sharp" 30 30 40 10
                '((child-frame-border-width . 2)))))
      (funcall fill-frame f1 "radius=0 sharp" "#330033" "#550055" "magenta"))
    ;; Frame 2: small radius (8)
    (when (fboundp 'neomacs-set-child-frame-style)
      (neomacs-set-child-frame-style :corner-radius 8 :shadow t
       :shadow-layers 4 :shadow-offset 2 :shadow-opacity 30))
    (let ((f2 (child-frame-test--make-child "small-radius" 30 250 40 10
                '((child-frame-border-width . 2)))))
      (funcall fill-frame f2 "radius=8 small" "#003300" "#005500" "lime green"))
    ;; Frame 3: large radius (20)
    (when (fboundp 'neomacs-set-child-frame-style)
      (neomacs-set-child-frame-style :corner-radius 20 :shadow t
       :shadow-layers 6 :shadow-offset 3 :shadow-opacity 40))
    (let ((f3 (child-frame-test--make-child "large-radius" 400 30 40 10
                '((child-frame-border-width . 3)))))
      (funcall fill-frame f3 "radius=20 large" "#000033" "#000055" "dodger blue"))
    ;; Frame 4: very round (30) + alpha
    (when (fboundp 'neomacs-set-child-frame-style)
      (neomacs-set-child-frame-style :corner-radius 30 :shadow t
       :shadow-layers 6 :shadow-offset 3 :shadow-opacity 40))
    (let ((f4 (child-frame-test--make-child "very-round" 400 250 40 10
                '((child-frame-border-width . 2) (alpha-background . 80)))))
      (funcall fill-frame f4 "radius=30 very round" "#332200" "#553300" "orange")))
  ;; Reset to reasonable default
  (when (fboundp 'neomacs-set-child-frame-style)
    (neomacs-set-child-frame-style :corner-radius 8 :shadow t
     :shadow-layers 4 :shadow-offset 2 :shadow-opacity 30))
  (message "Test 10: Rounded corners -- 4 frames: sharp, small, large, very-round -- check clipping"))

;; ============================================================================
;; Test runner
;; ============================================================================

(defvar child-frame-test--tests
  '(("Basic positioning" . child-frame-test--basic)
    ("Z-order stacking" . child-frame-test--z-order)
    ("Alpha transparency" . child-frame-test--alpha)
    ("Border styles" . child-frame-test--borders)
    ("Rich face content" . child-frame-test--rich-content)
    ("Cursor focus" . child-frame-test--cursor-focus)
    ("Style config" . child-frame-test--style-config)
    ("Rapid lifecycle" . child-frame-test--rapid-lifecycle)
    ("Tooltip" . child-frame-test--tooltip)
    ("Rounded corners" . child-frame-test--rounded-corners))
  "Alist of (name . function) for child frame tests.")

(defun child-frame-test-next ()
  "Run the next test in the sequence."
  (interactive)
  (when (>= child-frame-test--step (length child-frame-test--tests))
    (setq child-frame-test--step 0))
  (let ((test (nth child-frame-test--step child-frame-test--tests)))
    (message "--- Running test %d/%d: %s ---"
             (1+ child-frame-test--step)
             (length child-frame-test--tests)
             (car test))
    (funcall (cdr test))
    (setq child-frame-test--step (1+ child-frame-test--step))))

(defun child-frame-test-run-all ()
  "Run all tests sequentially with pauses.
Each test is preceded by explicit cleanup to ensure no stale frames remain."
  (interactive)
  (child-frame-test--cleanup)
  (setq child-frame-test--step 0)
  (let ((i 0))
    (dolist (test child-frame-test--tests)
      (run-at-time (* i 3) nil
                   (lambda (test-pair idx)
                     ;; Explicit cleanup before each test to guarantee
                     ;; no frames from the previous test linger.
                     (child-frame-test--cleanup)
                     (message "\n=== Test %d/%d: %s ==="
                              (1+ idx)
                              (length child-frame-test--tests)
                              (car test-pair))
                     (funcall (cdr test-pair)))
                   test i)
      (setq i (1+ i)))
    ;; Final cleanup
    (run-at-time (* i 3) nil
                 (lambda ()
                   (child-frame-test--cleanup)
                   (message "\n=== All %d child frame tests complete ===" (length child-frame-test--tests))
                   (message "Press 'q' to clean up, 'n' for individual test, 'a' to run all again"))))
  (message "Running all %d tests (3 second intervals)..." (length child-frame-test--tests)))

;; Keybindings for interactive use
(defvar child-frame-test-map (make-sparse-keymap)
  "Keymap for child frame test.")

(define-key child-frame-test-map (kbd "n") #'child-frame-test-next)
(define-key child-frame-test-map (kbd "a") #'child-frame-test-run-all)
(define-key child-frame-test-map (kbd "q") (lambda () (interactive)
                                             (child-frame-test--cleanup)
                                             (message "All child frames cleaned up.")))
(define-key child-frame-test-map (kbd "1") (lambda () (interactive) (child-frame-test--basic)))
(define-key child-frame-test-map (kbd "2") (lambda () (interactive) (child-frame-test--z-order)))
(define-key child-frame-test-map (kbd "3") (lambda () (interactive) (child-frame-test--alpha)))
(define-key child-frame-test-map (kbd "4") (lambda () (interactive) (child-frame-test--borders)))
(define-key child-frame-test-map (kbd "5") (lambda () (interactive) (child-frame-test--rich-content)))
(define-key child-frame-test-map (kbd "6") (lambda () (interactive) (child-frame-test--cursor-focus)))
(define-key child-frame-test-map (kbd "7") (lambda () (interactive) (child-frame-test--style-config)))
(define-key child-frame-test-map (kbd "8") (lambda () (interactive) (child-frame-test--rapid-lifecycle)))
(define-key child-frame-test-map (kbd "9") (lambda () (interactive) (child-frame-test--tooltip)))
(define-key child-frame-test-map (kbd "0") (lambda () (interactive) (child-frame-test--rounded-corners)))

(set-transient-map child-frame-test-map t)

;; Setup on load
(message "=== Child Frame Test Suite ===")
(message "Keys: n=next  a=run-all  1-9,0=specific test  q=cleanup")
(child-frame-test-run-all)

;;; child-frame-test.el ends here
