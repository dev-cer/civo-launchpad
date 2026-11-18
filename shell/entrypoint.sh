#!/bin/bash
# Civo Launchpad Shell: one pod per participant. Starts the kubeconfig watcher, then ttyd (web terminal).
#   SHELL_ID, SHELL_TOKEN, TTYD_CREDENTIAL, LAUNCHPAD_INTERNAL  (set by Launchpad when it creates the pod)
set -u
export HOME=/home/learner
mkdir -p "$HOME/.kube"
[ -d "$HOME/beyond-the-container" ] || cp -r /opt/beyond-the-container "$HOME/beyond-the-container"
[ -f "$HOME/.bashrc" ] || cp /etc/launchpad/bashrc "$HOME/.bashrc"
/usr/local/bin/launchpad-watch &
exec /usr/local/bin/ttyd -W -p 7681 -b "/shell/${SHELL_ID}" -c "${TTYD_CREDENTIAL}" \
  -t fontSize=15 -t 'theme={"background":"#0b0b0b","foreground":"#e8e8e8","cursor":"#00ff6f"}' \
  -t titleFixed="Civo Launchpad Shell" -t disableLeaveAlert=true \
  bash
# Note: never pass options to bash here - ttyd parses trailing options (e.g. -i) as its own and crashes.
