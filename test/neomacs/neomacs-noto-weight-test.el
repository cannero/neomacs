;;; neomacs-noto-weight-test.el --- Noto Sans Mono bold vs extra-bold test -*- lexical-binding: t -*-

;; Usage:
;;   emacs -Q -l test/neomacs/neomacs-noto-weight-test.el

;;; Commentary:

;; Minimal repro for comparing:
;;   - :family "Noto Sans Mono" :height 1.6 :weight bold
;;   - :family "Noto Sans Mono" :height 1.6 :weight extra-bold
;;
;; Focus text: a好好b

;;; Code:

(defun neomacs-noto-weight-test ()
  "Open a minimal buffer comparing bold vs extra-bold for Noto and Hack."
  (interactive)
  (let ((buf (get-buffer-create "*Neomacs Noto Weight Test*")))
    (switch-to-buffer buf)
    (setq buffer-read-only nil)
    (erase-buffer)

    (let ((s (point)))
      (insert "Noto Sans Mono + Hack h=1.6 weight comparison (a好好b)\n")
      (put-text-property s (point)
                         'face '(:weight bold :height 1.3 :foreground "cyan")))

    (insert "Move point through each line and compare glyph/cursor geometry.\n\n")

    (insert (format "  %-28s " "Noto h=1.6 w=bold:"))
    (let ((s (point)))
      (insert "a好好b  ABCXYZ 0123456789  -> <= >=")
      (put-text-property s (point)
                         'face '(:family "Noto Sans Mono" :height 1.6 :weight bold)))
    (insert "\n")

    (insert (format "  %-28s " "Noto h=1.6 w=extra-bold:"))
    (let ((s (point)))
      (insert "a好好b  ABCXYZ 0123456789  -> <= >=")
      (put-text-property s (point)
                         'face '(:family "Noto Sans Mono" :height 1.6 :weight extra-bold)))
    (insert "\n")

    ;; (insert (format "  %-28s " "Hack h=1.6 w=bold:"))
    ;; (let ((s (point)))
    ;;   (insert "a好好b  ABCXYZ 0123456789  -> <= >=")
    ;;   (put-text-property s (point)
    ;;                      'face '(:family "Hack" :height 1.6 :weight bold)))
    ;; (insert "\n")

    ;; (insert (format "  %-28s " "Hack h=1.6 w=extra-bold:"))
    ;; (let ((s (point)))
    ;;   (insert "a好好b  ABCXYZ 0123456789  -> <= >=")
    ;;   (put-text-property s (point)
    ;;                      'face '(:family "Hack" :height 1.6 :weight extra-bold)))
    ;; (insert "\n\n")

    (insert "\n")

    (insert "Tip: place cursor on first 好, then second 好, and compare starts/ends.\n")
    (goto-char (point-min))
    (forward-line 3)
    (search-forward "a")
    (setq buffer-read-only t)
    (message "Noto weight test buffer ready.")))

;; Run automatically when loaded.
(neomacs-noto-weight-test)

;;; neomacs-noto-weight-test.el ends here
