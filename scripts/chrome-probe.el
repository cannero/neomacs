;;; chrome-probe.el --- Deterministic GUI chrome probe -*- lexical-binding: t; -*-

(defvar neomacs-chrome-probe-ready-file
  (getenv "NEOMACS_CHROME_PROBE_READY_FILE")
  "Optional file path written after the chrome probe has been applied.")

(defvar neomacs-chrome-probe-header "Header sample"
  "Header-line text used by the GUI chrome probe.")

(defvar neomacs-chrome-probe--armed nil
  "Non-nil once the GUI chrome probe has been armed.")

(defun neomacs-chrome-probe--write-ready ()
  "Write a small readiness report when `neomacs-chrome-probe-ready-file' is set."
  (when neomacs-chrome-probe-ready-file
    (with-temp-file neomacs-chrome-probe-ready-file
      (prin1
       (list :frame-width (frame-pixel-width)
             :frame-height (frame-pixel-height)
             :frame-char-height (frame-char-height)
             :buffer (buffer-name (current-buffer))
             :header-line header-line-format
             :mode-line-height (window-mode-line-height)
             :header-line-height (window-header-line-height)
             :tab-line-height (window-tab-line-height)
             :tab-bar-lines (frame-parameter nil 'tab-bar-lines)
             :tab-bar-height (when (fboundp 'tab-bar-height)
                               (tab-bar-height))
             :default-text-height (when (fboundp 'default-text-height)
                                    (default-text-height))
             :line-spacing line-spacing)
       (current-buffer))
      (insert "\n"))))

(defun neomacs-chrome-probe-apply ()
  "Apply a consistent tab-bar/tab-line/header-line setup to the selected window."
  (interactive)
  (when (display-graphic-p)
    (tab-bar-mode 1)
    (global-tab-line-mode 1)
    (setq-default header-line-format neomacs-chrome-probe-header)
    (switch-to-buffer "*scratch*")
    (force-mode-line-update t)
    (redisplay)
    (run-with-timer 0 nil #'neomacs-chrome-probe--write-ready)))

(unless neomacs-chrome-probe--armed
  (setq neomacs-chrome-probe--armed t)
  (add-hook 'window-setup-hook #'neomacs-chrome-probe-apply)
  (when after-init-time
    (run-with-timer 0 nil #'neomacs-chrome-probe-apply)))

(provide 'chrome-probe)

;;; chrome-probe.el ends here
