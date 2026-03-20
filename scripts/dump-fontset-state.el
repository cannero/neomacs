;;; dump-fontset-state.el --- Write live GUI fontset state to a file -*- lexical-binding: t; -*-

(defvar neomacs-dump-fontset-state-file nil
  "Target file for the dumped fontset state.")

(defvar neomacs-dump-fontset-state-char "好"
  "Character string to probe when dumping fontset state.")

(defvar neomacs-dump-fontset-state-run-setup nil
  "Non-nil means call `setup-default-fontset' before writing the dump.")

(defvar neomacs-dump-fontset-state--armed nil
  "Non-nil once the dump helper has been armed.")

(defun neomacs-dump-fontset-state--configure-from-env ()
  "Populate helper variables from environment overrides."
  (when-let ((file (getenv "NEOMACS_DUMP_FONTSET_STATE_FILE")))
    (setq neomacs-dump-fontset-state-file file))
  (when-let ((text (getenv "NEOMACS_DUMP_FONTSET_STATE_CHAR")))
    (setq neomacs-dump-fontset-state-char text))
  (setq neomacs-dump-fontset-state-run-setup
        (member (getenv "NEOMACS_DUMP_FONTSET_STATE_RUN_SETUP")
                '("1" "true" "yes"))))

(defun neomacs-dump-fontset-state--write ()
  "Write the current GUI fontset state to `neomacs-dump-fontset-state-file'."
  (when (and neomacs-dump-fontset-state-file
             (display-graphic-p))
    (let* ((char (aref neomacs-dump-fontset-state-char 0))
           (setup-result
            (when neomacs-dump-fontset-state-run-setup
              (condition-case err
                  (progn
                    (setup-default-fontset)
                    'ok)
                (error (list 'error err))))))
      (with-temp-file neomacs-dump-fontset-state-file
        (prin1
         (list
          :display-graphic-p (display-graphic-p)
          :window-system (window-system)
          :initial-window-system initial-window-system
          :neomacs-initialized (bound-and-true-p neomacs-initialized)
          :feature-neomacs-win (featurep 'neomacs-win)
          :setup-default-fontset-result setup-result
          :default-fontset (query-fontset "fontset-default")
          :char char
          :fontset-font (fontset-font t char t))
         (current-buffer))))
    t))

(defun neomacs-dump-fontset-state-arm ()
  "Arm the one-shot fontset state dumper."
  (neomacs-dump-fontset-state--configure-from-env)
  (unless neomacs-dump-fontset-state--armed
    (setq neomacs-dump-fontset-state--armed t)
    (or (neomacs-dump-fontset-state--write)
        (add-hook 'emacs-startup-hook #'neomacs-dump-fontset-state--write)
        (add-hook 'window-setup-hook #'neomacs-dump-fontset-state--write))))

(neomacs-dump-fontset-state-arm)

(provide 'dump-fontset-state)

;;; dump-fontset-state.el ends here
