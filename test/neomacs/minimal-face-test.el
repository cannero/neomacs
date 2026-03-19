;;; minimal-face-test.el --- Minimal face test -*- lexical-binding: t -*-
;; Usage: ./target/release/neomacs -Q -l test/neomacs/minimal-face-test.el

(let ((buf (get-buffer-create "*Face Test*")))
  (switch-to-buffer buf)
  (erase-buffer)
  (insert "Normal text\n")
  (let ((s (point)))
    (insert "RED TEXT\n")
    (put-text-property s (point) 'face '(:foreground "red")))
  (let ((s (point)))
    (insert "BOLD TEXT\n")
    (put-text-property s (point) 'face '(:weight bold)))
  (let ((s (point)))
    (insert "GREEN BG\n")
    (put-text-property s (point) 'face '(:background "green" :foreground "black")))
  (let ((s (point)))
    (insert "BIG TEXT\n")
    (put-text-property s (point) 'face '(:height 2.0)))
  (let ((s (point)))
    (insert "UNDERLINE\n")
    (put-text-property s (point) 'face '(:underline t)))
  (insert "\nIf you see this, face test loaded OK.\n")
  (goto-char (point-min)))

;;; minimal-face-test.el ends here
