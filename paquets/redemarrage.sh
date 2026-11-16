        # Mise à jour : redémarrer plus tard, une fois apt et dpkg finis. La mise à jour peut être
        # lancée depuis un terminal Ricochet, que le redémarrage couperait en pleine installation.
        if [ -d /run/systemd/system ]; then
            systemd-run --quiet --collect --unit="UNITE-maj-$(date +%s)" /bin/sh -c \
                'sleep 3; while pidof -x apt apt-get dpkg unattended-upgrade >/dev/null; do sleep 2; done; systemctl try-restart UNITE.service' \
                >/dev/null 2>&1 \
                || echo "UNITE : redémarrer le service pour finir la mise à jour (systemctl restart UNITE)"
        fi
