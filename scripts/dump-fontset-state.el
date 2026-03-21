;;; dump-fontset-state.el --- Write live GUI fontset state to a file -*- lexical-binding: t; -*-

(defvar neomacs-dump-fontset-state-file nil
  "Target file for the dumped fontset state.")

(defvar neomacs-dump-fontset-state-char "好"
  "Character string to probe when dumping fontset state.")

(defvar neomacs-dump-fontset-state-charset-probes
  '(iso-8859-2 chinese-gb2312 unicode-bmp)
  "Charset symbols to probe with `encode-char' in the dump output.")

(defvar neomacs-dump-fontset-state-run-setup nil
  "Non-nil means call `setup-default-fontset' before writing the dump.")

(defvar neomacs-dump-fontset-state--armed nil
  "Non-nil once the dump helper has been armed.")

(defvar neomacs-dump-fontset-state--timer nil
  "Timer object for the currently scheduled fontset dump.")

(defvar neomacs-dump-fontset-state-delay 0.25
  "Seconds to wait before retrying the fontset dump.")

(defvar neomacs-dump-fontset-state-font-encoding-probes
  '("gb2312.1980"
    "jisx0208"
    "big5"
    "ksc5601.1987"
    "cns11643.1992.*1"
    "gb18030"
    "jisx0213.2000-1"
    "iso10646-1$")
  "Regex patterns to probe inside `font-encoding-alist'.")

(defun neomacs-dump-fontset-state--configure-from-env ()
  "Populate helper variables from environment overrides."
  (let ((file (getenv "NEOMACS_DUMP_FONTSET_STATE_FILE"))
        (text (getenv "NEOMACS_DUMP_FONTSET_STATE_CHAR")))
    (when file
      (setq neomacs-dump-fontset-state-file file))
    (when text
      (setq neomacs-dump-fontset-state-char text)))
  (setq neomacs-dump-fontset-state-run-setup
        (member (getenv "NEOMACS_DUMP_FONTSET_STATE_RUN_SETUP")
                '("1" "true" "yes"))))

(defun neomacs-dump-fontset-state--font-encoding-entry (pattern)
  "Return the first `font-encoding-alist' entry whose car equals PATTERN."
  (catch 'found
    (dolist (entry (and (boundp 'font-encoding-alist) font-encoding-alist))
      (when (and (consp entry)
                 (equal (car entry) pattern))
        (throw 'found entry)))
    nil))

(defun neomacs-dump-fontset-state--write ()
  "Write the current GUI fontset state to `neomacs-dump-fontset-state-file'."
  (when (and neomacs-dump-fontset-state-file
             (display-graphic-p))
    (let* ((char (aref neomacs-dump-fontset-state-char 0))
           (script-value (and (boundp 'char-script-table)
                              (char-table-range char-script-table char)))
           (charset-encodings
            (mapcar
             (lambda (charset)
               (cons charset
                     (and (fboundp 'encode-char)
                          (ignore-errors (encode-char char charset)))))
             neomacs-dump-fontset-state-charset-probes))
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
          :char-script-table-bound (boundp 'char-script-table)
          :char-script-value script-value
          :charset-script-alist-bound (boundp 'charset-script-alist)
          :charset-script-alist-size
          (and (boundp 'charset-script-alist)
               (length charset-script-alist))
          :font-encoding-alist-bound (boundp 'font-encoding-alist)
          :font-encoding-alist-size
          (and (boundp 'font-encoding-alist)
               (length font-encoding-alist))
          :font-encoding-probes
          (mapcar
           (lambda (pattern)
             (cons pattern
                   (and (boundp 'font-encoding-alist)
                        (neomacs-dump-fontset-state--font-encoding-entry pattern))))
           neomacs-dump-fontset-state-font-encoding-probes)
          :charset-encodings charset-encodings
          :default-fontset (query-fontset "fontset-default")
          :char char
          :fontset-font (fontset-font t char t))
         (current-buffer))))
    t))

(defun neomacs-dump-fontset-state--cancel-timer ()
  "Cancel the active dump timer, if any."
  (when (timerp neomacs-dump-fontset-state--timer)
    (cancel-timer neomacs-dump-fontset-state--timer))
  (setq neomacs-dump-fontset-state--timer nil))

(defun neomacs-dump-fontset-state--schedule ()
  "Schedule or immediately write the live GUI fontset dump."
  (neomacs-dump-fontset-state--cancel-timer)
  (unless (neomacs-dump-fontset-state--write)
    (setq neomacs-dump-fontset-state--timer
          (run-with-timer
           neomacs-dump-fontset-state-delay
           nil
           #'neomacs-dump-fontset-state--schedule))))

(defun neomacs-dump-fontset-state-arm ()
  "Arm the one-shot fontset state dumper."
  (neomacs-dump-fontset-state--configure-from-env)
  (unless neomacs-dump-fontset-state--armed
    (setq neomacs-dump-fontset-state--armed t)
    (unless (neomacs-dump-fontset-state--write)
      (add-hook 'emacs-startup-hook #'neomacs-dump-fontset-state--schedule)
      (add-hook 'window-setup-hook #'neomacs-dump-fontset-state--schedule)
      (when after-init-time
        (neomacs-dump-fontset-state--schedule)))))

(neomacs-dump-fontset-state-arm)

(provide 'dump-fontset-state)

;;; dump-fontset-state.el ends here
