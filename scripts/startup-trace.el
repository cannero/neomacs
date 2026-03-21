;;; startup-trace.el --- Trace shutdown-prone startup paths -*- lexical-binding: t; -*-

(defvar neomacs-startup-trace-file
  (or (getenv "NEOMACS_STARTUP_TRACE_FILE")
      "/tmp/neomacs-startup-trace.log")
  "File that receives startup trace lines.")

(defvar neomacs-startup-trace-enabled t
  "Non-nil enables startup tracing.")

(defun neomacs-startup-trace--write (fmt &rest args)
  "Append one formatted trace line to `neomacs-startup-trace-file'."
  (when neomacs-startup-trace-enabled
    (let ((line (concat (apply #'format fmt args) "\n")))
      (with-temp-buffer
        (insert line)
        (append-to-file (point-min) (point-max) neomacs-startup-trace-file)))))

(defun neomacs-startup-trace--safe (fn)
  "Call FN and turn any error into a printable marker."
  (condition-case err
      (funcall fn)
    (error (format "<error %S>" err))))

(defun neomacs-startup-trace--frame-snapshot ()
  "Return a compact snapshot of frame state for trace logging."
  (list
   :selected (neomacs-startup-trace--safe #'selected-frame)
   :live-selected
   (neomacs-startup-trace--safe
    (lambda ()
      (let ((frame (selected-frame)))
        (frame-live-p frame))))
   :frames (neomacs-startup-trace--safe #'frame-list)
   :visible (neomacs-startup-trace--safe #'visible-frame-list)
   :window-system window-system
   :initial-window-system initial-window-system
   :args-left command-line-args-left))

(defun neomacs-startup-trace--around-kill-emacs (orig &rest args)
  "Log kill-emacs calls before delegating to ORIG."
  (neomacs-startup-trace--write
   "kill-emacs args=%S snapshot=%S"
   args
   (neomacs-startup-trace--frame-snapshot))
  (apply orig args))

(defun neomacs-startup-trace--around-delete-frame (orig &rest args)
  "Log delete-frame calls before delegating to ORIG."
  (neomacs-startup-trace--write
   "delete-frame args=%S snapshot=%S"
   args
   (neomacs-startup-trace--frame-snapshot))
  (prog1
      (apply orig args)
    (neomacs-startup-trace--write
     "delete-frame return snapshot=%S"
     (neomacs-startup-trace--frame-snapshot))))

(defun neomacs-startup-trace--around-frame-live-p (orig object)
  "Log calls where `frame-live-p' returns nil."
  (let ((result (funcall orig object)))
    (when (null result)
      (neomacs-startup-trace--write
       "frame-live-p nil object=%S snapshot=%S"
       object
       (neomacs-startup-trace--frame-snapshot)))
    result))

(defun neomacs-startup-trace--mark (label)
  "Log LABEL with the current startup snapshot."
  (neomacs-startup-trace--write
   "%s snapshot=%S"
   label
   (neomacs-startup-trace--frame-snapshot)))

(defun neomacs-startup-trace--around-suspect (name orig &rest args)
  "Log startup-sensitive calls to NAME around ORIG with ARGS."
  (neomacs-startup-trace--write
   "enter %s args=%S snapshot=%S"
   name
   args
   (neomacs-startup-trace--frame-snapshot))
  (condition-case err
      (let ((result (apply orig args)))
        (neomacs-startup-trace--write
         "leave %s result=%S snapshot=%S"
         name
         result
         (neomacs-startup-trace--frame-snapshot))
        result)
    (error
     (neomacs-startup-trace--write
      "error %s err=%S snapshot=%S"
      name
      err
      (neomacs-startup-trace--frame-snapshot))
     (signal (car err) (cdr err)))))

(advice-add 'kill-emacs :around #'neomacs-startup-trace--around-kill-emacs)
(advice-add 'delete-frame :around #'neomacs-startup-trace--around-delete-frame)
(advice-add 'frame-live-p :around #'neomacs-startup-trace--around-frame-live-p)
(dolist (fn '(frame-set-background-mode
              frame--current-background-mode
              frame-parameter
              display-color-p
              xw-display-color-p
              x-display-grayscale-p))
  (when (fboundp fn)
    (advice-add fn :around
                (apply-partially #'neomacs-startup-trace--around-suspect fn))))

(add-hook 'emacs-startup-hook
          (lambda ()
            (neomacs-startup-trace--mark "emacs-startup-hook")))
(add-hook 'window-setup-hook
          (lambda ()
            (neomacs-startup-trace--mark "window-setup-hook")))

(neomacs-startup-trace--mark "startup-trace-loaded")

(provide 'startup-trace)

;;; startup-trace.el ends here
