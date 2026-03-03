;;; buffer-face-mode-bold-test.el --- Repro for bold + buffer-face-mode -*- lexical-binding: t; -*-

;; Usage:
;;   RUST_LOG=trace ./src/emacs -Q -l test/neomacs/buffer-face-mode-bold-test.el
;;
;; This test creates explicit bold text, then enables `buffer-face-mode'
;; (with `variable-pitch') and logs face state before/after toggling.

(require 'face-remap)

(defvar neomacs-buffer-face-test-buffer-name "*neomacs-buffer-face-bold*"
  "Buffer name used by the buffer-face-mode bold repro test.")

(defvar neomacs-buffer-face-test-log-file "/tmp/bfm-test.el.log"
  "File where the test writes deterministic state logs.")

(defun neomacs-buffer-face-test--emit (fmt &rest args)
  "Log a formatted line to both *Messages* and `neomacs-buffer-face-test-log-file'."
  (let ((line (apply #'format fmt args)))
    (message "%s" line)
    (with-temp-buffer
      (insert line "\n")
      (append-to-file (point-min) (point-max) neomacs-buffer-face-test-log-file))))

(defun neomacs-buffer-face-test--log-state (label pos)
  "Log face information at POS with LABEL."
  (save-excursion
    (goto-char pos)
    (let* ((tp (get-text-property pos 'face))
           (fp (face-at-point))
           (fp-weight (and fp (face-attribute fp :weight nil t)))
           (tp-weight (cond
                       ((symbolp tp) (face-attribute tp :weight nil t))
                       ((and (listp tp) (plist-member tp :weight))
                        (plist-get tp :weight))
                       (t 'unknown))))
      (neomacs-buffer-face-test--emit
       "[bfm-test] %s pos=%d buffer-face-mode=%S buffer-face-mode-face=%S textprop=%S textprop-weight=%S face-at-point=%S face-at-point-weight=%S"
       label pos buffer-face-mode buffer-face-mode-face tp tp-weight fp fp-weight))))

(defun neomacs-buffer-face-test-run ()
  "Open repro buffer and toggle `buffer-face-mode' with logging."
  (interactive)
  (let ((buf (get-buffer-create neomacs-buffer-face-test-buffer-name))
        p-bold-sym
        p-bold-plist)
    (switch-to-buffer buf)
    (setq buffer-read-only nil)
    (erase-buffer)
    (insert "Buffer-face-mode bold repro\n")
    (insert "Watch the two bold samples before and after enabling buffer-face-mode.\n\n")

    (insert "plain text sample\n")
    (setq p-bold-sym (point))
    (insert (propertize "bold-symbol sample (face 'bold)\n" 'face 'bold))
    (setq p-bold-plist (point))
    (insert (propertize "bold-plist sample (face '(:weight bold :foreground \"gold\"))\n"
                        'face '(:weight bold :foreground "gold")))
    (insert "\n")
    (insert "Commands:\n")
    (insert "  b -> toggle buffer-face-mode\n")
    (insert "  l -> log current face state\n")
    (insert "  q -> quit window\n")

    (goto-char p-bold-sym)
    (setq-local buffer-face-mode-face 'variable-pitch)
    (buffer-face-mode 0)
    (setq-local cursor-type t)

    (local-set-key (kbd "b")
                   (lambda ()
                     (interactive)
                     (buffer-face-mode (if buffer-face-mode 0 1))
                     (redisplay t)
                     (neomacs-buffer-face-test--log-state "manual:bold-symbol" p-bold-sym)
                     (neomacs-buffer-face-test--log-state "manual:bold-plist" p-bold-plist)))
    (local-set-key (kbd "l")
                   (lambda ()
                     (interactive)
                     (neomacs-buffer-face-test--log-state "manual:bold-symbol" p-bold-sym)
                     (neomacs-buffer-face-test--log-state "manual:bold-plist" p-bold-plist)))
    (local-set-key (kbd "q") #'quit-window)

    (when (file-exists-p neomacs-buffer-face-test-log-file)
      (delete-file neomacs-buffer-face-test-log-file))

    (neomacs-buffer-face-test--emit
     "[bfm-test] backend=%S default-family=%S variable-pitch-family=%S bold-weight=%S"
     (when (fboundp 'neomacs-core-backend) (neomacs-core-backend))
     (face-attribute 'default :family nil t)
     (face-attribute 'variable-pitch :family nil t)
     (face-attribute 'bold :weight nil t))

    ;; Baseline logs before enable.
    (neomacs-buffer-face-test--log-state "before-enable:bold-symbol" p-bold-sym)
    (neomacs-buffer-face-test--log-state "before-enable:bold-plist" p-bold-plist)

    ;; Auto-enable buffer-face-mode shortly after startup.
    (run-at-time
     "1 sec" nil
     (lambda ()
       (when (buffer-live-p buf)
         (with-current-buffer buf
           (buffer-face-mode 1)
           (redisplay t)
           (neomacs-buffer-face-test--log-state "after-enable:bold-symbol" p-bold-sym)
           (neomacs-buffer-face-test--log-state "after-enable:bold-plist" p-bold-plist)
           (neomacs-buffer-face-test--emit
            "[bfm-test] auto-enabled buffer-face-mode; press b to toggle, l to log")))))))

(neomacs-buffer-face-test-run)

;;; buffer-face-mode-bold-test.el ends here
