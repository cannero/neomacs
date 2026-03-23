;;; startup-state-dump.el --- Write selected startup state to a file -*- lexical-binding: t; -*-

(defvar neomacs-startup-state-file
  (or (getenv "NEOMACS_STARTUP_STATE_FILE")
      "/tmp/neomacs-startup-state.txt")
  "File that receives the startup state snapshot.")

(defvar neomacs-startup-state-delay
  (string-to-number
   (or (getenv "NEOMACS_STARTUP_STATE_DELAY")
       "0.5"))
  "Seconds to wait before writing the startup state snapshot.")

(defvar neomacs-startup-state--scheduled nil
  "Non-nil once the startup state dump has been scheduled.")

(defun neomacs-startup-state--window-end-safe (window)
  "Return `window-end' for WINDOW, or nil if it errors."
  (condition-case nil
      (window-end window nil t)
    (error nil)))

(defun neomacs-startup-state--visible-point-p (window point)
  "Return non-nil when POINT is visible in WINDOW."
  (condition-case nil
      (and (pos-visible-in-window-p point window t) t)
    (error nil)))

(defun neomacs-startup-state--maybe-call (fn &rest args)
  "Call FN with ARGS when it is bound, otherwise return nil."
  (when (fboundp fn)
    (condition-case nil
        (apply fn args)
      (error nil))))

(defun neomacs-startup-state--snapshot ()
  "Return a plist describing the selected startup state."
  (let* ((frame (selected-frame))
         (window (selected-window))
         (buffer (window-buffer window))
         (point (with-current-buffer buffer (point)))
         (point-min (with-current-buffer buffer (point-min)))
         (point-max (with-current-buffer buffer (point-max)))
         (sample-end (with-current-buffer buffer
                       (min point-max (+ point-min 200)))))
    (list
     :buffer (with-current-buffer buffer (buffer-name))
     :major-mode (with-current-buffer buffer major-mode)
     :mode-name (with-current-buffer buffer mode-name)
     :point point
     :point-min point-min
     :point-max point-max
     :buffer-size (with-current-buffer buffer (buffer-size))
     :window-start (window-start window)
     :window-end (neomacs-startup-state--window-end-safe window)
     :point-visible (neomacs-startup-state--visible-point-p window point)
     :window-body-size (list (window-body-width window t)
                             (window-body-height window t))
     :window-pixel-size (neomacs-startup-state--maybe-call
                         #'window-pixel-size window)
     :frame-char-size (neomacs-startup-state--maybe-call #'frame-size frame)
     :frame-pixel-size
     (let ((width (neomacs-startup-state--maybe-call
                   #'frame-pixel-width frame))
           (height (neomacs-startup-state--maybe-call
                    #'frame-pixel-height frame)))
       (and width height (list width height)))
     :window-system window-system
     :initial-window-system initial-window-system
     :buffer-sample
     (with-current-buffer buffer
       (buffer-substring-no-properties point-min sample-end)))))

(defun neomacs-startup-state--write ()
  "Write the startup state snapshot to `neomacs-startup-state-file'."
  (with-temp-file neomacs-startup-state-file
    (let ((print-length nil)
          (print-level nil))
      (prin1 (neomacs-startup-state--snapshot) (current-buffer))
      (insert "\n"))))

(defun neomacs-startup-state--write-safe ()
  "Write the startup state snapshot, turning errors into file output."
  (condition-case err
      (neomacs-startup-state--write)
    (error
     (with-temp-file neomacs-startup-state-file
       (let ((print-length nil)
             (print-level nil))
         (prin1 (list :error err) (current-buffer))
         (insert "\n"))))))

(defun neomacs-startup-state--schedule ()
  "Schedule a one-shot startup state dump."
  (unless neomacs-startup-state--scheduled
    (setq neomacs-startup-state--scheduled t)
    (run-at-time neomacs-startup-state-delay nil
                 #'neomacs-startup-state--write-safe)))

(add-hook 'emacs-startup-hook #'neomacs-startup-state--schedule)
(add-hook 'window-setup-hook #'neomacs-startup-state--schedule)

(neomacs-startup-state--schedule)

(provide 'startup-state-dump)

;;; startup-state-dump.el ends here
