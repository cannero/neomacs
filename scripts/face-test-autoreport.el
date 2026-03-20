;;; face-test-autoreport.el --- Write face probe reports after GUI setup -*- lexical-binding: t; -*-

(defvar neomacs-face-test-autoreport-file nil
  "Target file for `neomacs-face-test-write-matrix-report'.")

(defvar neomacs-face-test-autoreport-delay 0.5
  "Seconds to wait after GUI setup before writing the face probe report.")

(defvar neomacs-face-test-autoreport-poll-interval 0.25
  "Seconds between readiness checks while waiting to write the report.")

(defvar neomacs-face-test-autoreport-timeout 15.0
  "Maximum seconds to wait for expected GUI geometry before forcing a report.")

(defvar neomacs-face-test-autoreport-expected-frame-width nil
  "Expected `frame-pixel-width' before the report is considered ready.")

(defvar neomacs-face-test-autoreport-expected-frame-height nil
  "Expected `frame-pixel-height' before the report is considered ready.")

(defvar neomacs-face-test-autoreport--armed nil
  "Non-nil once the autoreport helper has been armed.")

(defvar neomacs-face-test-autoreport--deadline nil
  "Absolute timeout used by the current autoreport polling cycle.")

(defvar neomacs-face-test-autoreport--timer nil
  "Timer object for the currently scheduled autoreport polling cycle.")

(defvar neomacs-face-test-autoreport-debug nil
  "Non-nil means log helper scheduling and geometry decisions.")

(defun neomacs-face-test-autoreport--log (fmt &rest args)
  "Log a helper debug message when `neomacs-face-test-autoreport-debug' is non-nil."
  (when neomacs-face-test-autoreport-debug
    (apply #'message (concat "face-test-autoreport: " fmt) args)))

(defun neomacs-face-test-autoreport--buffer-ready-p ()
  "Return non-nil when the GUI face test buffer can be probed."
  (and neomacs-face-test-autoreport-file
       (display-graphic-p)
       (fboundp 'neomacs-face-test-write-matrix-report)
       (get-buffer "*Neomacs Face Test*")))

(defun neomacs-face-test-autoreport--geometry-ready-p ()
  "Return non-nil when the selected frame reached the expected pixel geometry."
  (and (or (null neomacs-face-test-autoreport-expected-frame-width)
           (= (frame-pixel-width) neomacs-face-test-autoreport-expected-frame-width))
       (or (null neomacs-face-test-autoreport-expected-frame-height)
           (= (frame-pixel-height) neomacs-face-test-autoreport-expected-frame-height))))

(defun neomacs-face-test-autoreport--deadline-expired-p ()
  "Return non-nil when the current autoreport wait deadline has expired."
  (and neomacs-face-test-autoreport--deadline
       (not (time-less-p (current-time) neomacs-face-test-autoreport--deadline))))

(defun neomacs-face-test-autoreport--cancel-timer ()
  "Cancel the active autoreport timer, if any."
  (when (timerp neomacs-face-test-autoreport--timer)
    (neomacs-face-test-autoreport--log
     "cancel timer=%S frame=%sx%s"
     neomacs-face-test-autoreport--timer
     (frame-pixel-width)
     (frame-pixel-height))
    (cancel-timer neomacs-face-test-autoreport--timer))
  (setq neomacs-face-test-autoreport--timer nil))

(defun neomacs-face-test-autoreport--write ()
  "Write the face probe report when the GUI face test is ready.
Return non-nil when the report was written."
  (when (and (neomacs-face-test-autoreport--buffer-ready-p)
             (or (neomacs-face-test-autoreport--geometry-ready-p)
                 (neomacs-face-test-autoreport--deadline-expired-p)))
    (neomacs-face-test-autoreport--log
     "write report ready=%S geometry=%S deadline-expired=%S frame=%sx%s expected=%sx%s"
     (neomacs-face-test-autoreport--buffer-ready-p)
     (neomacs-face-test-autoreport--geometry-ready-p)
     (neomacs-face-test-autoreport--deadline-expired-p)
     (frame-pixel-width)
     (frame-pixel-height)
     (or neomacs-face-test-autoreport-expected-frame-width "?")
     (or neomacs-face-test-autoreport-expected-frame-height "?"))
    (unless (neomacs-face-test-autoreport--geometry-ready-p)
      (message
       "face-test-autoreport: timed out waiting for %sx%s, writing report at %sx%s"
       (or neomacs-face-test-autoreport-expected-frame-width "?")
       (or neomacs-face-test-autoreport-expected-frame-height "?")
       (frame-pixel-width)
       (frame-pixel-height)))
    (neomacs-face-test-autoreport--cancel-timer)
    (neomacs-face-test-write-matrix-report neomacs-face-test-autoreport-file)
    t))

(defun neomacs-face-test-autoreport--poll ()
  "Continue polling until the face probe report can be written."
  (neomacs-face-test-autoreport--log
   "poll ready=%S geometry=%S deadline-expired=%S frame=%sx%s expected=%sx%s"
   (neomacs-face-test-autoreport--buffer-ready-p)
   (neomacs-face-test-autoreport--geometry-ready-p)
   (neomacs-face-test-autoreport--deadline-expired-p)
   (frame-pixel-width)
   (frame-pixel-height)
   (or neomacs-face-test-autoreport-expected-frame-width "?")
   (or neomacs-face-test-autoreport-expected-frame-height "?"))
  (unless (neomacs-face-test-autoreport--write)
    (setq neomacs-face-test-autoreport--timer
          (run-with-timer
           neomacs-face-test-autoreport-poll-interval
           nil
           #'neomacs-face-test-autoreport--poll))))

(defun neomacs-face-test-autoreport--schedule ()
  "Schedule one deferred face probe report write."
  (when neomacs-face-test-autoreport-file
    (setq neomacs-face-test-autoreport--deadline
          (time-add (current-time)
                    (seconds-to-time neomacs-face-test-autoreport-timeout)))
    (neomacs-face-test-autoreport--log
     "schedule frame=%sx%s expected=%sx%s after-init-time=%S startup-file=%S"
     (frame-pixel-width)
     (frame-pixel-height)
     (or neomacs-face-test-autoreport-expected-frame-width "?")
     (or neomacs-face-test-autoreport-expected-frame-height "?")
     after-init-time
     neomacs-face-test-autoreport-file)
    (neomacs-face-test-autoreport--cancel-timer)
    (if (neomacs-face-test-autoreport--write)
        t
      (setq neomacs-face-test-autoreport--timer
            (run-with-timer
             neomacs-face-test-autoreport-delay
             nil
             #'neomacs-face-test-autoreport--poll)))))

(defun neomacs-face-test-autoreport-arm ()
  "Arm the face probe autoreport helper once."
  (unless neomacs-face-test-autoreport--armed
    (setq neomacs-face-test-autoreport--armed t)
    (neomacs-face-test-autoreport--log
     "arm ready=%S after-init-time=%S display=%S file=%S"
     (neomacs-face-test-autoreport--buffer-ready-p)
     after-init-time
     (display-graphic-p)
     neomacs-face-test-autoreport-file)
    (unless (neomacs-face-test-autoreport--write)
      (add-hook 'emacs-startup-hook #'neomacs-face-test-autoreport--schedule)
      (add-hook 'window-setup-hook #'neomacs-face-test-autoreport--schedule)
      (neomacs-face-test-autoreport--log
       "installed startup hooks; after-init-time=%S" after-init-time)
      (when after-init-time
        (neomacs-face-test-autoreport--schedule)))))

(provide 'face-test-autoreport)

;;; face-test-autoreport.el ends here
