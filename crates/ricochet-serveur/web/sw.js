// Service worker de Ricochet : affiche les notifications poussées par le relais et ouvre
// l'interface sur le bon message au toucher. Il ne met rien en cache et n'intercepte aucune
// requête. Le contenu poussé se limite à : id, titre, texte, machine, genre.
"use strict";

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (evenement) => evenement.waitUntil(self.clients.claim()));

self.addEventListener("push", (evenement) => {
  let donnees = {};
  try {
    donnees = evenement.data ? evenement.data.json() : {};
  } catch {
    donnees = {};
  }
  const chaine = (valeur) => (typeof valeur === "string" ? valeur : "");
  const titre = chaine(donnees.titre) || "Ricochet";
  const corps = [chaine(donnees.texte), chaine(donnees.machine)].filter(Boolean).join(" · ");
  const id = chaine(donnees.id);
  evenement.waitUntil(
    self.registration.showNotification(titre, {
      body: corps,
      // Une notification par message : un nouvel envoi du même message remplace l'ancien.
      tag: id || "ricochet",
      icon: "/icone-192.png",
      badge: "/icone-192.png",
      data: { id },
    }),
  );
});

self.addEventListener("notificationclick", (evenement) => {
  evenement.notification.close();
  const id = (evenement.notification.data && evenement.notification.data.id) || "";
  // La notification d'essai n'a pas de message : on ouvre simplement l'interface.
  const message = id && id !== "essai" ? id : "";
  const adresse = message ? `/#message/${encodeURIComponent(message)}` : "/";
  evenement.waitUntil(
    (async () => {
      const fenetres = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
      for (const fenetre of fenetres) {
        if (new URL(fenetre.url).origin === self.location.origin) {
          // Interface déjà ouverte : au premier plan, puis elle affiche le message.
          if ("focus" in fenetre) await fenetre.focus();
          fenetre.postMessage({ type: "ouvrir-message", id: message });
          return;
        }
      }
      await self.clients.openWindow(adresse);
    })(),
  );
});
