#!/bin/bash
# Civo Launchpad Shell: one pod per participant. Starts the kubeconfig watcher, then ttyd (web terminal).
#   SHELL_ID, SHELL_TOKEN, TTYD_CREDENTIAL, LAUNCHPAD_INTERNAL  (set by Launchpad when it creates the pod)
set -u
export HOME=/home/learner
mkdir -p "$HOME/.kube"
# Latest workshop repo at start (falls back to the copy baked into the image).
if [ ! -d "$HOME/beyond-the-container" ]; then
  timeout 20 git clone -q --depth 1 https://github.com/dev-cer/beyond-the-container "$HOME/beyond-the-container" 2>/dev/null \
    || cp -r /opt/beyond-the-container "$HOME/beyond-the-container"
fi
[ -f "$HOME/.bashrc" ] || cp /etc/launchpad/bashrc "$HOME/.bashrc"
/usr/local/bin/launchpad-watch &
exec /usr/local/bin/ttyd -W -p 7681 -b "/shell/${SHELL_ID}" -c "${TTYD_CREDENTIAL}" \
  -t fontSize=15 -t 'theme={"background":"#0b0b0b","foreground":"#e8e8e8","cursor":"#00ff6f"}' \
  -t titleFixed="Civo Launchpad Shell" -t disableLeaveAlert=true \
  bash
# Note: never pass options to bash here - ttyd parses trailing options (e.g. -i) as its own and crashes.
