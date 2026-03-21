;;; scratch-probe.el --- Dump scratch buffer font-lock state -*- lexical-binding: t; -*-

(defvar neomacs-scratch-probe-output
  (or (getenv "NEOMACS_SCRATCH_PROBE_OUTPUT")
      "/tmp/neomacs-scratch-probe.sexp")
  "Output file for the scratch-buffer startup probe.")

(defvar neomacs-scratch-probe-delay
  (string-to-number
   (or (getenv "NEOMACS_SCRATCH_PROBE_DELAY")
       "0.5"))
  "Seconds to wait before writing the scratch-buffer probe.")

(defvar neomacs-scratch-probe--armed nil
  "Non-nil once the probe has been armed.")

(defun neomacs-scratch-probe--comment-pos ()
  "Return the first comment position in `*scratch*', or nil."
  (with-current-buffer "*scratch*"
    (save-excursion
      (goto-char (point-min))
      (when (search-forward ";;" nil t)
        (match-beginning 0)))))

(defun neomacs-scratch-probe--char-state (pos)
  "Return a plist describing POS in `*scratch*'."
  (when pos
    (with-current-buffer "*scratch*"
      (list :pos pos
            :char (char-after pos)
            :face (get-text-property pos 'face)
            :font-lock-face (get-text-property pos 'font-lock-face)
            :char-face (get-char-property pos 'face)
            :props (text-properties-at pos)))))

(defun neomacs-scratch-probe--snapshot ()
  "Return a plist describing the live `*scratch*' state."
  (with-current-buffer "*scratch*"
    (let* ((comment-pos (neomacs-scratch-probe--comment-pos))
           (sample-end (min (point-max) (+ (point-min) 120)))
           (raw-comment (neomacs-scratch-probe--char-state comment-pos))
           (raw-next (neomacs-scratch-probe--char-state (and comment-pos (1+ comment-pos)))))
      (when (fboundp 'font-lock-ensure)
        (font-lock-ensure (point-min) (point-max)))
      (list
       :major-mode major-mode
       :mode-name mode-name
       :global-font-lock (bound-and-true-p global-font-lock-mode)
       :font-lock font-lock-mode
       :jit (bound-and-true-p jit-lock-mode)
       :defaults font-lock-defaults
       :char-syntax-comment (and comment-pos (char-syntax (char-after comment-pos)))
       :syntax-entry-comment (and comment-pos (syntax-after comment-pos))
       :syntax-ppss-comment (and comment-pos (syntax-ppss comment-pos))
       :syntax-ppss-after-comment (and comment-pos (syntax-ppss (1+ comment-pos)))
       :buffer-sample (buffer-substring-no-properties (point-min) sample-end)
       :raw-comment raw-comment
       :raw-next raw-next
       :ensured-comment (neomacs-scratch-probe--char-state comment-pos)
       :ensured-next (neomacs-scratch-probe--char-state (and comment-pos (1+ comment-pos)))))))

(defun neomacs-scratch-probe--write ()
  "Write the current `*scratch*' snapshot to disk."
  (when (get-buffer "*scratch*")
    (with-temp-file neomacs-scratch-probe-output
      (let ((print-length nil)
            (print-level nil))
        (prin1 (neomacs-scratch-probe--snapshot) (current-buffer))
        (insert "\n")))))

(defun neomacs-scratch-probe-arm ()
  "Arm the one-shot scratch-buffer probe."
  (unless neomacs-scratch-probe--armed
    (setq neomacs-scratch-probe--armed t)
    (run-at-time neomacs-scratch-probe-delay nil
                 #'neomacs-scratch-probe--write)))

(add-hook 'emacs-startup-hook #'neomacs-scratch-probe-arm)
(add-hook 'window-setup-hook #'neomacs-scratch-probe-arm)

(when after-init-time
  (neomacs-scratch-probe-arm))

(provide 'scratch-probe)

;;; scratch-probe.el ends here
