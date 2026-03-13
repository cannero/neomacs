;;; bold-overlap-test.el --- Repro for bold face text overlap -*- lexical-binding: t; -*-

;; Usage:
;;   RUST_LOG=debug ./src/emacs -Q -l test/neomacs/bold-overlap-test.el
;;
;; Reproduces the corfu-current bold overlap issue:
;; - corfu-default (normal weight) renders fine
;; - corfu-current (bold weight) overlaps
;;
;; This test creates a child frame popup mimicking corfu's completion menu,
;; with alternating normal and bold lines, plus standalone comparison buffers.

;;; Code:

(defface bold-overlap-test-normal
  '((t (:height 1.0
        :foreground "white"
        :weight normal
        :background "black"
        :family "JetBrainsMono Nerd Font")))
  "Normal weight face (like corfu-default).")

(defface bold-overlap-test-bold
  '((t (:background "purple"
        :foreground "white"
        :weight bold)))
  "Bold weight face (like corfu-current).")

(defvar bold-overlap-test--frames nil)

(defun bold-overlap-test--cleanup ()
  "Delete all test frames."
  (dolist (f bold-overlap-test--frames)
    (when (frame-live-p f)
      (delete-frame f)))
  (setq bold-overlap-test--frames nil)
  (redisplay t))

(defun bold-overlap-test-run ()
  "Create test buffers and child frame to reproduce bold overlap."
  (interactive)
  (bold-overlap-test--cleanup)

  ;; --- Main buffer: side-by-side comparison ---
  (let ((buf (get-buffer-create "*bold-overlap-test*")))
    (switch-to-buffer buf)
    (erase-buffer)

    (insert (propertize "Bold Overlap Test\n\n"
                        'face '(:height 1.5 :weight bold :foreground "gold")))

    (insert "=== Normal weight (corfu-default style) ===\n")
    (dotimes (i 10)
      (insert (propertize (format "  completion-item-%d   some-extra-text-here\n" (1+ i))
                          'face 'bold-overlap-test-normal)))

    (insert "\n=== Bold weight (corfu-current style) ===\n")
    (dotimes (i 10)
      (insert (propertize (format "  completion-item-%d   some-extra-text-here\n" (1+ i))
                          'face 'bold-overlap-test-bold)))

    (insert "\n=== Alternating normal/bold (like corfu menu) ===\n")
    (dotimes (i 10)
      (let ((face (if (= i 3) 'bold-overlap-test-bold 'bold-overlap-test-normal)))
        (insert (propertize (format "  %s  completion-item-%d   extra-text\n"
                                    (if (= i 3) ">>" "  ")
                                    (1+ i))
                            'face face))))

    (insert "\n=== Raw bold face ===\n")
    (insert (propertize "  normal weight line for reference\n"
                        'face '(:family "JetBrainsMono Nerd Font" :weight normal :foreground "white")))
    (insert (propertize "  bold weight line -- check for overlap\n"
                        'face '(:family "JetBrainsMono Nerd Font" :weight bold :foreground "yellow")))
    (insert (propertize "  normal weight line after bold\n"
                        'face '(:family "JetBrainsMono Nerd Font" :weight normal :foreground "white")))

    (goto-char (point-min)))

  ;; --- Child frame: mimics corfu popup ---
  (when (fboundp 'neomacs-set-child-frame-style)
    (neomacs-set-child-frame-style :corner-radius 8 :shadow t
     :shadow-layers 4 :shadow-offset 2 :shadow-opacity 30))

  ;; Set the frame font so bold faces inherit the correct family
  (set-frame-font "JetBrainsMono Nerd Font-13" nil t)

  (let* ((child-buf (get-buffer-create "*bold-overlap-child*"))
         (f (make-frame
             `((parent-frame . ,(selected-frame))
               (left . 500) (top . 80)
               (width . 45) (height . 14)
               (minibuffer . nil)
               (no-accept-focus . t)
               (child-frame-border-width . 1)
               (internal-border-width . 4)
               (undecorated . t)
               (font . "JetBrainsMono Nerd Font-13")
               (visibility . t)))))
    (push f bold-overlap-test--frames)
    (with-selected-frame f
      (switch-to-buffer child-buf)
      (erase-buffer)
      (face-remap-add-relative 'default
                               :background "black"
                               :foreground "white")
      ;; Simulate corfu completion list
      (dotimes (i 12)
        (let* ((selected (= i 3))
               (face (if selected 'bold-overlap-test-bold 'bold-overlap-test-normal))
               (prefix (if selected "> " "  "))
               (text (format "%s%-30s  %s\n"
                             prefix
                             (nth (mod i 8) '("accept" "access" "account"
                                              "achieve" "across" "action"
                                              "activity" "actually"))
                             (if selected "<method>" "<func>"))))
          (insert (propertize text 'face face))))))

  (message "Bold overlap test ready -- RUST_LOG=debug for layout diagnostics")
  (message "Compare normal vs bold lines in both main buffer and child frame popup")
  (message "Press 'q' to cleanup"))

;; Keybinding
(defvar bold-overlap-test-map (make-sparse-keymap))
(define-key bold-overlap-test-map (kbd "q")
  (lambda () (interactive)
    (bold-overlap-test--cleanup)
    (when (get-buffer "*bold-overlap-test*")
      (kill-buffer "*bold-overlap-test*"))
    (when (get-buffer "*bold-overlap-child*")
      (kill-buffer "*bold-overlap-child*"))
    (message "Bold overlap test cleaned up.")))
(define-key bold-overlap-test-map (kbd "r")
  (lambda () (interactive) (bold-overlap-test-run)))
(set-transient-map bold-overlap-test-map t)

(bold-overlap-test-run)

;;; bold-overlap-test.el ends here
