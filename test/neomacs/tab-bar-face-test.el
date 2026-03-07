;;; tab-bar-face-test.el --- Test tab-bar face attributes rendering -*- lexical-binding: t -*-

;; Test that the tab-bar GPU overlay correctly renders all face attributes:
;; font family, foreground/background colors, weight, italic, underline,
;; overline, strike-through, and box decorations.
;;
;; Usage: RUST_LOG=debug ./src/emacs -Q -l test/neomacs/tab-bar-face-test.el
;;
;; What to check:
;; - Tab bar text should use the correct font family and weight
;; - Foreground/background colors should match what's set
;; - Underline, overline, strike-through should be visible when enabled
;; - Box borders should appear when set
;; - Character spacing should look normal (not too wide or too narrow)
;;
;; The test cycles through different face configurations every 3 seconds.
;; Check the RUST_LOG=debug output for SetTabBar and render_tab_bar messages.

;;; Code:

(defvar tab-bar-face-test-configs
  '(;; 1. Default: just enable tab-bar-mode
    (:name "default"
     :attrs nil)
    ;; 2. Custom fg/bg colors
    (:name "red-fg blue-bg"
     :attrs (:foreground "red" :background "midnight blue"))
    ;; 3. Bold weight
    (:name "bold"
     :attrs (:weight bold))
    ;; 4. Italic
    (:name "italic"
     :attrs (:slant italic))
    ;; 5. Underline
    (:name "underline"
     :attrs (:underline t))
    ;; 6. Colored underline
    (:name "colored underline"
     :attrs (:underline (:color "green" :style line)))
    ;; 7. Overline
    (:name "overline"
     :attrs (:overline t))
    ;; 8. Strike-through
    (:name "strike-through"
     :attrs (:strike-through t))
    ;; 9. Box
    (:name "box"
     :attrs (:box (:line-width 2 :color "green")))
    ;; 10. Large height
    (:name "height 1.3"
     :attrs (:height 1.3))
    ;; 11. Small height
    (:name "height 0.8"
     :attrs (:height 0.8))
    ;; 12. Combined: bold + underline + colored
    (:name "bold+underline+yellow"
     :attrs (:weight bold :underline t :foreground "yellow" :background "dark green"))
    ;; 13. Different font family
    (:name "serif family"
     :attrs (:family "serif"))
    ;; 14. All decorations at once
    (:name "all decorations"
     :attrs (:weight bold :underline t :overline t :strike-through t
             :foreground "cyan" :background "dark red"
             :box (:line-width 1 :color "yellow"))))
  "List of tab-bar face configurations to cycle through.")

(defvar tab-bar-face-test-index 0
  "Current index into `tab-bar-face-test-configs'.")

(defvar tab-bar-face-test-timer nil
  "Timer for cycling through configurations.")

(defun tab-bar-face-test-apply-config (config)
  "Apply CONFIG to the tab-bar face."
  (let ((name (plist-get config :name))
        (attrs (plist-get config :attrs)))
    ;; Reset tab-bar face to defaults first
    (set-face-attribute 'tab-bar nil
                        :foreground 'unspecified
                        :background 'unspecified
                        :weight 'normal
                        :slant 'normal
                        :underline nil
                        :overline nil
                        :strike-through nil
                        :box nil
                        :height 'unspecified
                        :family 'unspecified)
    ;; Apply new attributes
    (when attrs
      (apply #'set-face-attribute 'tab-bar nil attrs))
    (message "Tab-bar face test [%d/%d]: %s  attrs=%S"
             (1+ tab-bar-face-test-index)
             (length tab-bar-face-test-configs)
             name attrs)
    ;; Force redisplay to send updated face to renderer
    (force-mode-line-update t)
    (redisplay t)))

(defun tab-bar-face-test-next ()
  "Apply the next tab-bar face configuration."
  (let ((config (nth tab-bar-face-test-index tab-bar-face-test-configs)))
    (tab-bar-face-test-apply-config config)
    (setq tab-bar-face-test-index
          (mod (1+ tab-bar-face-test-index)
               (length tab-bar-face-test-configs)))))

(defun tab-bar-face-test-stop ()
  "Stop the tab-bar face test timer."
  (interactive)
  (when tab-bar-face-test-timer
    (cancel-timer tab-bar-face-test-timer)
    (setq tab-bar-face-test-timer nil))
  (message "Tab-bar face test stopped."))

(defun tab-bar-face-test ()
  "Run the tab-bar face attribute test.
Cycles through various face configurations every 3 seconds.
Use `tab-bar-face-test-stop' or M-x tab-bar-face-test-stop to stop."
  ;; Ensure tab-bar-mode is active with a few tabs
  (tab-bar-mode 1)

  ;; Create some tabs so we have content to display
  (switch-to-buffer (get-buffer-create "*Tab Bar Face Test*"))
  (erase-buffer)
  (insert "Tab Bar Face Attribute Test\n")
  (insert "===========================\n\n")
  (insert "This test cycles through different tab-bar face configurations.\n")
  (insert "Watch the tab bar at the top and check RUST_LOG=debug output.\n\n")
  (insert "Configurations being tested:\n")
  (let ((i 1))
    (dolist (config tab-bar-face-test-configs)
      (insert (format "  %2d. %s\n" i (plist-get config :name)))
      (setq i (1+ i))))
  (insert "\nPress C-c C-k or M-x tab-bar-face-test-stop to stop.\n")
  (goto-char (point-min))

  ;; Create a second tab
  (tab-bar-new-tab)
  (switch-to-buffer (get-buffer-create "*Tab 2*"))
  (insert "Second tab for testing.\n")
  ;; Switch back to first tab
  (tab-bar-switch-to-prev-tab)

  ;; Start cycling
  (setq tab-bar-face-test-index 0)
  (tab-bar-face-test-next)
  (setq tab-bar-face-test-timer
        (run-with-timer 3 3 #'tab-bar-face-test-next))
  (message "Tab-bar face test started. Cycling every 3s. M-x tab-bar-face-test-stop to stop."))

;; Bind stop key
(global-set-key (kbd "C-c C-k") #'tab-bar-face-test-stop)

;; Run the test
(tab-bar-face-test)

;;; tab-bar-face-test.el ends here
