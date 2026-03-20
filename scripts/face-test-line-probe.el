;;; face-test-line-probe.el --- Dump face-test line geometry after GUI startup -*- lexical-binding: t; -*-

(defvar neomacs-face-test-probe-line 60
  "1-based source line number to probe in `*Neomacs Face Test*'.")

(defvar neomacs-face-test-probe-output
  (or (getenv "NEOMACS_FACE_TEST_PROBE_OUTPUT")
      "/tmp/neomacs-face-line-probe.txt")
  "Output path for the probe report.")

(defvar neomacs-face-test-probe-prime-lines nil
  "Optional list of 1-based line numbers to visit before the final probe line.")

(defvar neomacs-face-test-probe-delay 0.5
  "Seconds to wait before the first probe attempt.")

(defvar neomacs-face-test-probe-poll-interval 0.25
  "Seconds between retries while waiting for the GUI window.")

(defvar neomacs-face-test-probe-timeout 15.0
  "Maximum seconds to wait before forcing a probe write.")

(defvar neomacs-face-test-probe-expected-frame-width nil
  "Expected `frame-pixel-width' before the probe is considered ready.")

(defvar neomacs-face-test-probe-expected-frame-height nil
  "Expected `frame-pixel-height' before the probe is considered ready.")

(defvar neomacs-face-test-probe--armed nil
  "Non-nil once the helper has been armed.")

(defvar neomacs-face-test-probe--deadline nil
  "Absolute timeout for the current probe attempt.")

(defvar neomacs-face-test-probe--timer nil
  "Timer object for the currently scheduled probe attempt.")

(defun neomacs-face-test-line-probe--cancel-timer ()
  "Cancel the active probe timer, if any."
  (when (timerp neomacs-face-test-probe--timer)
    (cancel-timer neomacs-face-test-probe--timer))
  (setq neomacs-face-test-probe--timer nil))

(defun neomacs-face-test-line-probe--buffer-ready-p ()
  "Return non-nil when the face test buffer is ready for probing."
  (and (display-graphic-p)
       (get-buffer "*Neomacs Face Test*")
       (get-buffer-window "*Neomacs Face Test*" 0)))

(defun neomacs-face-test-line-probe--deadline-expired-p ()
  "Return non-nil when the probe wait deadline has elapsed."
  (and neomacs-face-test-probe--deadline
       (not (time-less-p (current-time) neomacs-face-test-probe--deadline))))

(defun neomacs-face-test-line-probe--geometry-ready-p ()
  "Return non-nil when the selected frame reached the expected pixel geometry."
  (and (or (null neomacs-face-test-probe-expected-frame-width)
           (= (frame-pixel-width) neomacs-face-test-probe-expected-frame-width))
       (or (null neomacs-face-test-probe-expected-frame-height)
           (= (frame-pixel-height) neomacs-face-test-probe-expected-frame-height))))

(defun neomacs-face-test-line-probe--char-info (pos win)
  "Return plist describing POS in WIN."
  (let* ((posn (posn-at-point pos win))
         (next-posn (and posn (posn-at-point (1+ pos) win)))
         (xy (and posn (posn-x-y posn)))
         (col-row (and posn (posn-col-row posn)))
         (next-xy (and next-posn (posn-x-y next-posn)))
         (x1 (and xy (car xy)))
         (x2 (and next-xy (car next-xy))))
    (list :pos pos
          :char (char-after pos)
          :font (when (fboundp 'font-at)
                  (font-at pos win))
          :advance (when (and (numberp x1) (numberp x2))
                     (- x2 x1))
          :x-y xy
          :col-row col-row
          :visible (pos-visible-in-window-p pos win t)
          :posn posn)))

(defun neomacs-face-test-line-probe--write ()
  "Write the requested line probe report when the GUI is ready.
Return non-nil when the report was written."
  (when (and (neomacs-face-test-line-probe--buffer-ready-p)
             (or (neomacs-face-test-line-probe--geometry-ready-p)
                 (neomacs-face-test-line-probe--deadline-expired-p))
             (get-buffer "*Neomacs Face Test*"))
    (let* ((src (get-buffer "*Neomacs Face Test*"))
           (win (or (get-buffer-window src 0)
                    (selected-window)))
           beg end sample chars line-posn probe-data)
      (with-current-buffer src
        (dolist (prime-line neomacs-face-test-probe-prime-lines)
          (goto-char (point-min))
          (forward-line (1- prime-line))
          (set-window-point win (line-beginning-position))
          (redisplay t))
        (goto-char (point-min))
        (forward-line (1- neomacs-face-test-probe-line))
        (setq beg (line-beginning-position))
        (setq end (line-end-position))
        (setq sample
              (save-excursion
                (goto-char beg)
                (unless (search-forward "a好好b" end t)
                  (error "Line %d does not contain a好好b" neomacs-face-test-probe-line))
                (match-beginning 0)))
        (setq probe-data (neomacs-face-test--probe-line beg end win))
        (setq line-posn (posn-at-point beg win))
        (setq chars
              (mapcar
               (lambda (pos)
                 (neomacs-face-test-line-probe--char-info pos win))
               (number-sequence sample (+ sample 5)))))
      (neomacs-face-test-line-probe--cancel-timer)
      (with-temp-file neomacs-face-test-probe-output
        (insert (format "line=%S beg=%S end=%S sample=%S ws=%S we=%S point=%S\n"
                        neomacs-face-test-probe-line
                        beg
                        end
                        sample
                        (window-start win)
                        (window-end win t)
                        (window-point win)))
        (insert (format "frame=%Sx%S frame-edges=%S\n"
                        (frame-pixel-width)
                        (frame-pixel-height)
                        (frame-edges)))
        (insert (format "selected-frame=%S window-system=%S frame-window-system=%S native=%Sx%S\n"
                        (selected-frame)
                        (window-system)
                        (frame-parameter nil 'window-system)
                        (frame-native-width)
                        (frame-native-height)))
        (insert (format "frame-parameters=%S\n"
                        (frame-parameters)))
        (insert (format "window-pixel-edges=%S inside-pixel-edges=%S body=%Sx%S\n"
                        (window-pixel-edges win)
                        (window-inside-pixel-edges win)
                        (window-body-width win t)
                        (window-body-height win t)))
        (insert (format "line-posn=%S line-text=%S\n"
                        line-posn
                        (with-current-buffer src
                          (buffer-substring-no-properties beg end))))
        (insert (format "probe-summary=%S\n" probe-data))
        (dolist (info chars)
          (insert (format "%S\n" info))))
      t)))

(defun neomacs-face-test-line-probe--poll ()
  "Continue polling until the probe can be written."
  (unless (neomacs-face-test-line-probe--write)
    (setq neomacs-face-test-probe--timer
          (run-with-timer
           neomacs-face-test-probe-poll-interval
           nil
           #'neomacs-face-test-line-probe--poll))))

(defun neomacs-face-test-line-probe-arm ()
  "Arm the line probe helper once."
  (unless neomacs-face-test-probe--armed
    (setq neomacs-face-test-probe--armed t)
    (setq neomacs-face-test-probe--deadline
          (time-add (current-time)
                    (seconds-to-time neomacs-face-test-probe-timeout)))
    (unless (neomacs-face-test-line-probe--write)
      (add-hook 'emacs-startup-hook #'neomacs-face-test-line-probe--poll)
      (add-hook 'window-setup-hook #'neomacs-face-test-line-probe--poll)
      (when after-init-time
        (setq neomacs-face-test-probe--timer
              (run-with-timer
               neomacs-face-test-probe-delay
               nil
               #'neomacs-face-test-line-probe--poll))))))

(defun neomacs-face-test-line-probe--write-on-size-change (_frame)
  "Write the probe report in response to a frame size change."
  (when (neomacs-face-test-line-probe--write)
    (remove-hook 'window-size-change-functions
                 #'neomacs-face-test-line-probe--write-on-size-change)))

(defun neomacs-face-test-line-probe-install-size-change-hook ()
  "Install a one-shot size-change hook that writes the probe report."
  (add-hook 'window-size-change-functions
            #'neomacs-face-test-line-probe--write-on-size-change))

(provide 'face-test-line-probe)

;;; face-test-line-probe.el ends here
