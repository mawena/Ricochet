// Interface de Ricochet. Les données reçues ne passent jamais par innerHTML : seulement
// textContent, pour qu'un nom de machine ne puisse pas injecter de code.
"use strict";

const PERIODE_MS = 10000;
const $ = (selecteur) => document.querySelector(selecteur);
let minuterie = null;
// Mode Portier (connexion unique) : adresses du bouton et de la déconnexion, ou null.
let portier = null;

class ErreurApi extends Error {
  constructor(statut, texte) {
    super(texte || `erreur ${statut}`);
    this.statut = statut;
  }
}

async function api(methode, chemin, corps) {
  const options = {
    method: methode,
    credentials: "same-origin",
    headers: { "X-Ricochet": "1" },
  };
  if (corps instanceof Blob) {
    options.headers["Content-Type"] = "application/octet-stream";
    options.body = corps;
  } else if (corps !== undefined) {
    options.headers["Content-Type"] = "application/json";
    options.body = JSON.stringify(corps);
  }
  const reponse = await fetch("/api" + chemin, options);
  if (!reponse.ok) {
    if (reponse.status === 401 && chemin !== "/connexion") {
      afficherConnexion();
    }
    throw new ErreurApi(reponse.status, await reponse.text());
  }
  const type = reponse.headers.get("Content-Type") || "";
  return type.includes("json") ? reponse.json() : null;
}

function message(texte, erreur = false) {
  const zone = $("#message");
  zone.textContent = texte;
  zone.classList.toggle("erreur", erreur);
  zone.hidden = !texte;
}

// --- Mise en forme ---

function octets(n) {
  const unites = ["o", "Ko", "Mo", "Go", "To"];
  let i = 0;
  while (n >= 1024 && i < unites.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(i >= 3 ? 1 : 0)} ${unites[i]}`;
}

function duree(secondes) {
  const j = Math.floor(secondes / 86400);
  const h = Math.floor((secondes % 86400) / 3600);
  const m = Math.floor((secondes % 3600) / 60);
  if (j > 0) return `${j} j ${h} h`;
  if (h > 0) return `${h} h ${m} min`;
  return `${m} min`;
}

function ilYa(instant) {
  if (!instant) return "jamais";
  const ecart = Math.max(0, Math.floor(Date.now() / 1000) - instant);
  if (ecart < 60) return "à l'instant";
  return `il y a ${duree(ecart)}`;
}

function date(instant) {
  return new Date(instant * 1000).toLocaleString("fr-FR", {
    day: "2-digit", month: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit",
  });
}

function element(balise, classe, texte) {
  const e = document.createElement(balise);
  if (classe) e.className = classe;
  if (texte !== undefined) e.textContent = texte;
  return e;
}

function jauge(fraction) {
  const barre = element("div", "jauge");
  const rempli = element("span");
  const pourcent = Math.min(100, Math.max(0, fraction * 100));
  rempli.style.width = `${pourcent}%`;
  if (pourcent >= 90) barre.classList.add("critique");
  else if (pourcent >= 75) barre.classList.add("haute");
  barre.append(rempli);
  return barre;
}

// --- Écrans ---

function afficherConnexion() {
  clearInterval(minuterie);
  minuterie = null;
  $("#ecran-tableau").hidden = true;
  $("#identite").hidden = true;
  $("#ecran-connexion").hidden = false;
}

function afficherTableau(compte) {
  $("#compte").textContent = compte;
  $("#identite").hidden = false;
  $("#ecran-connexion").hidden = true;
  $("#ecran-tableau").hidden = false;
  chargerMachines();
  majPastille();
  clearInterval(minuterie);
  minuterie = setInterval(() => {
    if (document.hidden || terminal || fichiers) return;
    if (!$("#onglet-machines").hidden) chargerMachines();
    else if (!$("#onglet-messages").hidden && !messageOuvert) chargerMessages();
    else majPastille();
  }, PERIODE_MS);
  // Ouverture par le toucher d'une notification : l'adresse porte l'identifiant du message.
  surChangementAdresse();
}

// --- Machines ---

function carteMachine(m) {
  const carte = $("#modele-machine").content.firstElementChild.cloneNode(true);
  carte.dataset.id = m.id;
  carte.querySelector(".pastille").classList.add(m.connectee ? "en-ligne" : "hors-ligne");
  carte.querySelector(".pastille").title = m.connectee ? "connectée" : "déconnectée";
  carte.querySelector(".nom").textContent = m.nom || m.id;
  const sousTitre = [m.nom ? m.id : null, m.info?.nom_hote, m.info?.systeme]
    .filter(Boolean)
    .join(" · ");
  carte.querySelector(".sous-titre").textContent = sousTitre;

  const mesures = carte.querySelector(".mesures");
  const ajouter = (titre, valeur, fraction) => {
    const bloc = element("div");
    bloc.append(element("dt", null, titre), element("dd", null, valeur));
    if (fraction !== undefined) bloc.append(jauge(fraction));
    mesures.append(bloc);
  };
  const e = m.etat;
  if (e) {
    ajouter("Processeur", `${e.cpu_pourcent.toFixed(0)} % (${e.nombre_coeurs} cœurs)`,
      e.cpu_pourcent / 100);
    ajouter("Mémoire", `${octets(e.memoire_utilisee)} / ${octets(e.memoire_totale)}`,
      e.memoire_totale ? e.memoire_utilisee / e.memoire_totale : 0);
    ajouter("Charge", e.charge.map((c) => c.toFixed(2)).join("  "));
    ajouter("Allumée depuis", duree(e.allumee_depuis_s));
    const disques = carte.querySelector(".disques");
    for (const d of e.disques) {
      const ligne = element("div", "disque");
      ligne.append(
        element("span", "point", d.point_montage),
        element("span", "taille", `${octets(d.utilise)} / ${octets(d.total)}`),
        jauge(d.total ? d.utilise / d.total : 0),
      );
      disques.append(ligne);
    }
  } else {
    mesures.append(element("p", "vide", "Pas encore d'état reçu."));
  }

  const vu = m.connectee
    ? `Connectée ${ilYa(m.connectee_le)} depuis ${m.adresse} · état ${ilYa(m.etat_recu_le)}`
    : `Déconnectée · vue ${ilYa(m.vue_le)}`;
  carte.querySelector(".vu").textContent = vu;

  const boutonTerminal = carte.querySelector(".ouvrir-terminal");
  boutonTerminal.hidden = !m.info?.terminal;
  boutonTerminal.disabled = !m.connectee;
  boutonTerminal.addEventListener("click", () => ouvrirTerminal(m));
  const boutonFichiers = carte.querySelector(".ouvrir-fichiers");
  boutonFichiers.hidden = !m.info?.fichiers;
  boutonFichiers.disabled = !m.connectee;
  boutonFichiers.addEventListener("click", () => ouvrirFichiers(m));

  // Applications web déclarées par l'agent : ouvertes en plein écran (lien dans la même page).
  const applis = m.info?.applis ?? [];
  carte.querySelector(".applis").hidden = applis.length === 0;
  for (const a of applis) {
    const lien = element("a", "bouton-lien", a.libelle || a.nom);
    lien.href = `/api/machines/${encodeURIComponent(m.id)}/applis/${encodeURIComponent(a.nom)}/`;
    if (!m.connectee) {
      lien.setAttribute("aria-disabled", "true");
      lien.removeAttribute("href");
    }
    carte.querySelector(".liste-applis").append(lien);
  }

  const actualiser = carte.querySelector(".actualiser");
  actualiser.disabled = !m.connectee;
  actualiser.addEventListener("click", () => actualiserMachine(m.id, actualiser));
  carte.querySelector(".revoquer").addEventListener("click", () => revoquer(m));
  return carte;
}

async function chargerMachines() {
  try {
    const machines = await api("GET", "/machines");
    $("#machines").replaceChildren(...machines.map(carteMachine));
    $("#aucune-machine").hidden = machines.length > 0;
  } catch (erreur) {
    if (erreur.statut !== 401) message(`Machines : ${erreur.message}`, true);
  }
}

async function actualiserMachine(id, bouton) {
  bouton.disabled = true;
  try {
    await api("POST", `/machines/${encodeURIComponent(id)}/actualiser`);
    setTimeout(chargerMachines, 1500);
  } catch (erreur) {
    message(erreur.message, true);
    bouton.disabled = false;
  }
}

async function revoquer(m) {
  const nom = m.nom || m.id;
  if (!confirm(`Révoquer « ${nom} » ? Son jeton ne marchera plus et sa connexion sera coupée.`)) {
    return;
  }
  try {
    await api("DELETE", `/machines/${encodeURIComponent(m.id)}`);
    message(`« ${nom} » révoquée.`);
    chargerMachines();
  } catch (erreur) {
    message(erreur.message, true);
  }
}

// Commande à coller sur la nouvelle machine : installe l'agent, écrit sa configuration
// (droits 600, elle contient le jeton) et le démarre. Une seule liste « && » : le shell lit
// tout avant de lancer sudo, dont la demande de mot de passe n'avale donc pas la suite.
function commandeInstallation(relais, cree, utilisateur) {
  const terminal = utilisateur
    ? `\n[terminal]\ncommande = ["runuser", "-l", "${utilisateur}"]\nmaximum = 8\n` +
      `\n[fichiers]\nutilisateur = "${utilisateur}"\n`
    : "";
  return (
    `curl -fsSL https://apt.mawena.cloud/installer.sh | sudo sh -s -- ricochet-agent &&\n` +
    `sudo install -D -m 600 /dev/null /etc/ricochet/agent.toml &&\n` +
    `sudo tee /etc/ricochet/agent.toml >/dev/null <<'EOF' &&\n` +
    `relais = "${relais}"\nid = "${cree.id}"\njeton = "${cree.jeton}"\n${terminal}EOF\n` +
    `sudo systemctl restart ricochet-agent\n`
  );
}

async function ajouterMachine(evenement) {
  evenement.preventDefault();
  const formulaire = evenement.target;
  // `formulaire.id` serait l'attribut id du formulaire : passer par `elements`.
  const id = formulaire.elements.namedItem("id").value.trim();
  const nom = formulaire.elements.namedItem("nom").value.trim();
  // Le motif du champ ne laisse passer qu'un nom d'utilisateur Unix : rien à échapper.
  const utilisateur = formulaire.elements.namedItem("utilisateur").value.trim();
  try {
    const cree = await api("POST", "/machines", { id, nom: nom || null });
    const relais = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/agent`;
    $("#nouveau-id").textContent = cree.id;
    $("#nouvelle-config").textContent = commandeInstallation(relais, cree, utilisateur);
    $("#nouveau-jeton").hidden = false;
    formulaire.reset();
    $("#ajout").open = false;
    message("");
    chargerMachines();
  } catch (erreur) {
    message(erreur.message, true);
  }
}

async function copierConfig() {
  try {
    await navigator.clipboard.writeText($("#nouvelle-config").textContent);
    $("#copier").textContent = "Copié ✓";
  } catch {
    // Presse-papiers refusé : sélectionner le texte pour une copie à la main.
    const plage = document.createRange();
    plage.selectNodeContents($("#nouvelle-config"));
    getSelection().removeAllRanges();
    getSelection().addRange(plage);
  }
}

function fermerJeton() {
  $("#nouvelle-config").textContent = "";
  $("#nouveau-jeton").hidden = true;
  $("#copier").textContent = "Copier";
}

// --- Terminal ---
// xterm.js n'est chargé qu'à la première ouverture d'un terminal : l'interface reste légère.

const SEQUENCES = {
  echap: "\x1b", tab: "\t", haut: "\x1b[A", bas: "\x1b[B", droite: "\x1b[C", gauche: "\x1b[D",
  pipe: "|", tilde: "~", tiret: "-", barre: "/",
};
let terminal = null; // { xterm, ajustement, socket, observateur }
let ctrlActif = false;
let chargementXterm = null;

function chargerScript(src) {
  return new Promise((resoudre, rejeter) => {
    const script = document.createElement("script");
    script.src = src;
    script.onload = resoudre;
    script.onerror = () => rejeter(new Error(`chargement de ${src} impossible`));
    document.head.append(script);
  });
}

function chargerXterm() {
  if (!chargementXterm) {
    const style = document.createElement("link");
    style.rel = "stylesheet";
    style.href = "/vendor/xterm.css";
    document.head.append(style);
    chargementXterm = Promise.all([
      chargerScript("/vendor/xterm.js"),
      chargerScript("/vendor/addon-fit.js"),
    ]).catch((erreur) => {
      chargementXterm = null;
      throw erreur;
    });
  }
  return chargementXterm;
}

function basculerCtrl(actif) {
  ctrlActif = actif;
  $("#touche-ctrl").classList.toggle("actif", actif);
}

/// Avec Ctrl enfoncé (bouton de l'écran), une lettre devient le caractère de contrôle.
function appliquerCtrl(donnees) {
  if (!ctrlActif || donnees.length !== 1) return donnees;
  basculerCtrl(false);
  const code = donnees.toUpperCase().charCodeAt(0);
  return code >= 64 && code <= 95 ? String.fromCharCode(code - 64) : donnees;
}

function envoyerAuTerminal(texte) {
  if (terminal?.socket.readyState === WebSocket.OPEN) {
    terminal.socket.send(new TextEncoder().encode(texte));
  }
}

async function ouvrirTerminal(m) {
  if (terminal) return;
  try {
    await chargerXterm();
  } catch (erreur) {
    message(erreur.message, true);
    return;
  }
  $("#titre-terminal").textContent = m.nom || m.id;
  $("#etat-terminal").textContent = "connexion…";
  $("#ecran-terminal").hidden = false;
  document.body.classList.add("terminal-ouvert");
  basculerCtrl(false);

  const xterm = new Terminal({
    cursorBlink: true,
    fontFamily: "ui-monospace, 'DejaVu Sans Mono', Menlo, monospace",
    fontSize: window.innerWidth < 600 ? 13 : 15,
    scrollback: 5000,
    theme: { background: "#0b1020" },
  });
  const ajustement = new FitAddon.FitAddon();
  xterm.loadAddon(ajustement);
  xterm.open($("#terminal"));
  ajustement.fit();

  const schema = location.protocol === "https:" ? "wss" : "ws";
  const url = `${schema}://${location.host}/api/machines/${encodeURIComponent(m.id)}/terminal` +
    `?colonnes=${xterm.cols}&lignes=${xterm.rows}`;
  const socket = new WebSocket(url);
  socket.binaryType = "arraybuffer";
  const observateur = new ResizeObserver(() => ajustement.fit());
  observateur.observe($("#terminal"));
  terminal = { xterm, ajustement, socket, observateur };

  socket.onopen = () => {
    $("#etat-terminal").textContent = "";
    xterm.focus();
  };
  socket.onmessage = (evenement) => {
    if (evenement.data instanceof ArrayBuffer) xterm.write(new Uint8Array(evenement.data));
  };
  socket.onclose = (evenement) => {
    const raison = evenement.reason || "connexion perdue";
    $("#etat-terminal").textContent = `fermé : ${raison}`;
    xterm.write(`\r\n\x1b[90m[terminal fermé : ${raison}]\x1b[0m\r\n`);
  };
  xterm.onData((donnees) => envoyerAuTerminal(appliquerCtrl(donnees)));
  xterm.onResize(({ cols, rows }) => {
    if (socket.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify({ colonnes: cols, lignes: rows }));
    }
  });
}

function fermerTerminal() {
  if (!terminal) return;
  terminal.observateur.disconnect();
  terminal.socket.onclose = null;
  terminal.socket.close();
  terminal.xterm.dispose();
  terminal = null;
  $("#ecran-terminal").hidden = true;
  document.body.classList.remove("terminal-ouvert");
  chargerMachines();
}

function toucheEcran(nom) {
  if (!terminal) return;
  if (nom === "ctrl") basculerCtrl(!ctrlActif);
  else envoyerAuTerminal(appliquerCtrl(SEQUENCES[nom]));
  terminal.xterm.focus();
}

// --- Fichiers ---
// Les envois partent par morceaux ; un envoi interrompu reprend où il s'est arrêté.
const MORCEAU = 512 * 1024;
let fichiers = null; // { machine, chemin, noms, occupe }

const urlFichiers = (id) => `/machines/${encodeURIComponent(id)}/fichiers`;
const joindre = (dossier, nom) => (dossier.endsWith("/") ? dossier : dossier + "/") + nom;
const parent = (chemin) => chemin.replace(/\/[^/]*\/?$/, "") || "/";

function etatFichiers(texte, erreur = false) {
  $("#etat-fichiers").textContent = texte;
  $("#etat-fichiers").classList.toggle("erreur", erreur);
}

function dateFichier(instant) {
  return new Date(instant * 1000).toLocaleString("fr-FR", {
    day: "2-digit", month: "2-digit", year: "numeric", hour: "2-digit", minute: "2-digit",
  });
}

function ouvrirFichiers(m) {
  fichiers = { machine: m, chemin: "~", noms: new Set(), occupe: false };
  $("#titre-fichiers").textContent = `Fichiers · ${m.nom || m.id}`;
  $("#liste-fichiers").replaceChildren();
  $("#ecran-fichiers").hidden = false;
  document.body.classList.add("fichiers-ouverts");
  chargerDossier("~");
}

function fermerFichiers() {
  if (!fichiers) return;
  fichiers = null;
  $("#ecran-fichiers").hidden = true;
  document.body.classList.remove("fichiers-ouverts");
}

function ligneEntree(e, dossier, url) {
  const dossierCible = e.genre === "dossier";
  const chemin = joindre(dossier, e.nom);
  let entree;
  if (dossierCible) {
    entree = element("button", "entree");
    entree.type = "button";
    entree.addEventListener("click", () => chargerDossier(chemin));
  } else {
    entree = element("a", "entree");
    entree.href = `/api${url}/telecharger?chemin=${encodeURIComponent(chemin)}`;
    entree.download = e.nom;
  }
  const icone = dossierCible ? "📁" : e.genre === "fichier" ? "📄" : "◽";
  const details = [dossierCible ? null : octets(e.taille), e.modifie_le ? dateFichier(e.modifie_le) : null]
    .filter(Boolean)
    .join(" · ");
  entree.append(
    element("span", "icone", e.lien ? "🔗" : icone),
    element("span", "nom-entree", e.nom),
    element("span", "details", details),
  );
  const ligne = element("li");
  ligne.append(entree);
  return ligne;
}

async function chargerDossier(chemin) {
  if (!fichiers) return;
  const session = fichiers;
  const url = urlFichiers(session.machine.id);
  etatFichiers("Chargement…");
  try {
    const d = await api("GET", `${url}?chemin=${encodeURIComponent(chemin)}`);
    if (fichiers !== session) return;
    session.chemin = d.chemin;
    session.noms = new Set(d.entrees.map((e) => e.nom));
    $("#chemin-fichiers").value = d.chemin;
    $("#liste-fichiers").replaceChildren(...d.entrees.map((e) => ligneEntree(e, d.chemin, url)));
    $("#liste-fichiers").scrollTop = 0;
    const n = d.entrees.length;
    etatFichiers(d.tronque ? `${n} premiers éléments seulement` : `${n} élément${n > 1 ? "s" : ""}`);
  } catch (erreur) {
    if (fichiers !== session) return;
    // Le champ revient au dossier affiché, celui où « Envoyer ici » écrira.
    $("#chemin-fichiers").value = session.chemin;
    etatFichiers(erreur.message, true);
  }
}

async function envoyerFichier(session, f) {
  const url = urlFichiers(session.machine.id);
  const cible = joindre(session.chemin, f.name);
  const parametres = (decalage, fin) =>
    `${url}?chemin=${encodeURIComponent(cible)}&decalage=${decalage}&fin=${fin}`;
  if (session.noms.has(f.name) && !confirm(`Remplacer « ${f.name} » ?`)) return false;
  let decalage = 0;
  const partiel = await api("GET", `${url}/partiel?chemin=${encodeURIComponent(cible)}`);
  if (partiel.taille > 0 && partiel.taille < f.size &&
      confirm(`Reprendre l'envoi de « ${f.name} » à ${octets(partiel.taille)} ?`)) {
    decalage = partiel.taille;
  }
  for (;;) {
    const morceau = f.slice(decalage, decalage + MORCEAU);
    const attendu = decalage + morceau.size;
    const fin = attendu >= f.size;
    const r = await api("PUT", parametres(decalage, fin), morceau);
    if (r.taille > f.size) throw new Error(`« ${f.name} » : fichier partiel plus gros que l'original`);
    if (fin && r.taille === attendu) return true;
    // Morceau accepté : on continue ; sinon l'agent dit où reprendre.
    decalage = r.taille;
    const pourcent = f.size ? Math.floor((100 * decalage) / f.size) : 100;
    etatFichiers(`Envoi de « ${f.name} » : ${pourcent} %`);
  }
}

async function envoyerFichiers(liste) {
  if (!fichiers || fichiers.occupe || !liste.length) return;
  const session = fichiers;
  session.occupe = true;
  $("#envoyer-fichiers").disabled = true;
  let envoyes = 0;
  try {
    for (const f of liste) {
      etatFichiers(`Envoi de « ${f.name} »…`);
      if (await envoyerFichier(session, f)) envoyes += 1;
      if (fichiers !== session) return;
    }
    await chargerDossier(session.chemin);
    etatFichiers(`${envoyes} fichier${envoyes > 1 ? "s" : ""} envoyé${envoyes > 1 ? "s" : ""}`);
  } catch (erreur) {
    if (fichiers === session) {
      etatFichiers(`Envoi interrompu (${erreur.message}) : renvoyer le fichier pour reprendre`, true);
    }
  } finally {
    session.occupe = false;
    $("#envoyer-fichiers").disabled = false;
  }
}

// --- Journal ---

const ACTIONS = {
  connexion: "Connexion",
  connexion_portier: "Connexion par Portier",
  connexion_refusee: "Connexion refusée",
  connexion_bloquee: "Connexion bloquée (trop d'essais)",
  deconnexion: "Déconnexion",
  machine_ajoutee: "Machine ajoutée",
  machine_revoquee: "Machine révoquée",
  etat_demande: "État demandé",
  agent_connecte: "Agent connecté",
  agent_deconnecte: "Agent déconnecté",
  agent_refuse: "Agent refusé",
  terminal_ouvert: "Terminal ouvert",
  terminal_ferme: "Terminal fermé",
  fichier_recu: "Fichier envoyé sur la machine",
  fichier_telecharge: "Fichier téléchargé",
  message_recu: "Message reçu",
  message_detail: "Détail de message consulté",
  message_reponse: "Réponse à un message",
  message_refuse: "Message refusé",
  message_limite: "Messages trop fréquents (pas de notification)",
  push_abonnement_ajoute: "Appareil abonné aux notifications",
  push_abonnement_retire: "Appareil retiré des notifications",
  push_essai: "Notification d'essai",
  compte_cree: "Compte créé",
  compte_supprime: "Compte supprimé",
};

async function chargerJournal() {
  try {
    const lignes = await api("GET", "/audit?limite=200");
    $("#journal").replaceChildren(...lignes.map((l) => {
      const li = element("li", l.action.includes("refus") || l.action.includes("bloq")
        ? "alerte" : "");
      const titre = ACTIONS[l.action] || l.action;
      li.append(
        element("time", null, date(l.instant)),
        element("strong", null, l.detail ? `${titre} : ${l.detail}` : titre),
        element("span", "qui", `${l.compte || "—"} · ${l.adresse}`),
      );
      return li;
    }));
  } catch (erreur) {
    if (erreur.statut !== 401) message(`Journal : ${erreur.message}`, true);
  }
}

// --- Messages des machines ---

const GENRES = { info: "Info", action: "Action", alerte: "Alerte" };
let messageOuvert = null;
let nonLus = 0;

function majCompteur(n) {
  nonLus = n;
  const pastille = $("#pastille-messages");
  pastille.textContent = n > 99 ? "99+" : String(n);
  pastille.hidden = n === 0;
}

function ligneMessage(m) {
  const li = element("li", `genre-${m.genre}${m.lu_le ? "" : " non-lu"}`);
  li.tabIndex = 0;
  li.append(element("span", "titre", m.titre));
  if (m.texte) li.append(element("p", "texte", m.texte));
  const reponse = m.reponse ? ` · réponse : ${libelleBouton(m, m.reponse.bouton)}` : "";
  li.append(element("p", "meta",
    `${GENRES[m.genre] || m.genre} · ${m.machine} · ${m.source} · ${ilYa(m.recu_le)}${reponse}`));
  const ouvrir = () => ouvrirMessage(m.id);
  li.addEventListener("click", ouvrir);
  li.addEventListener("keydown", (e) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      ouvrir();
    }
  });
  return li;
}

function libelleBouton(m, id) {
  return m.boutons.find((b) => b.id === id)?.libelle || id;
}

async function chargerMessages() {
  try {
    const rendu = await api("GET", "/messages?limite=100");
    majCompteur(rendu.non_lus);
    $("#liste-messages").replaceChildren(...rendu.messages.map(ligneMessage));
    $("#aucun-message").hidden = rendu.messages.length > 0;
  } catch (erreur) {
    if (erreur.statut !== 401) message(`Messages : ${erreur.message}`, true);
  }
}

// Seule la pastille des non lus est tenue à jour hors de l'onglet Messages.
async function majPastille() {
  try {
    const rendu = await api("GET", "/messages?limite=1");
    majCompteur(rendu.non_lus);
  } catch {
    // Silencieux : relais plus ancien ou session expirée.
  }
}

function fermerMessage() {
  messageOuvert = null;
  $("#vue-detail-message").hidden = true;
  $("#vue-liste-messages").hidden = false;
  if (location.hash.startsWith("#message/")) {
    history.replaceState(null, "", location.pathname + location.search);
  }
  chargerMessages();
}

async function ouvrirMessage(id) {
  changerOnglet("messages", false);
  try {
    const m = await api("GET", `/messages/${encodeURIComponent(id)}`);
    messageOuvert = m;
    $("#vue-liste-messages").hidden = true;
    $("#vue-detail-message").hidden = false;
    $("#detail-titre").textContent = m.titre;
    $("#detail-meta").textContent =
      `${GENRES[m.genre] || m.genre} · ${m.machine} · ${m.source} · ${date(m.recu_le)}`;
    $("#detail-texte").textContent = m.texte;
    afficherReponse(m);
    const boutons = $("#detail-boutons");
    boutons.replaceChildren(...m.boutons.map((b) => {
      const bouton = element("button", null, b.libelle);
      bouton.type = "button";
      bouton.dataset.bouton = b.id;
      bouton.disabled = Boolean(m.reponse);
      bouton.addEventListener("click", () => repondreMessage(m, b.id));
      return bouton;
    }));
    $("#detail-bloc").hidden = !m.detail;
    if (m.detail) chargerDetail(m.id);
    majPastille();
  } catch (erreur) {
    if (erreur.statut === 404) {
      message("Ce message n'existe plus (purgé ?).", true);
      fermerMessage();
    } else if (erreur.statut !== 401) {
      message(`Message : ${erreur.message}`, true);
    }
  }
}

function afficherReponse(m) {
  const zone = $("#detail-reponse");
  if (!m.reponse) {
    zone.hidden = true;
    return;
  }
  const etat = m.reponse.livree ? "transmise à la machine" : "en attente de la machine";
  zone.textContent = `Réponse envoyée : ${libelleBouton(m, m.reponse.bouton)} ` +
    `(${m.reponse.par}, ${date(m.reponse.quand)}) · ${etat}`;
  zone.hidden = false;
}

// Le détail n'est pas stocké par le relais : il est demandé à la machine à chaque affichage.
async function chargerDetail(id) {
  const etat = $("#detail-etat");
  const contenu = $("#detail-contenu");
  contenu.hidden = true;
  $("#copier-detail").hidden = true;
  $("#recharger-detail").hidden = true;
  etat.textContent = "Lecture du détail sur la machine…";
  try {
    const reponse = await fetch(`/api/messages/${encodeURIComponent(id)}/detail`, {
      credentials: "same-origin",
      headers: { "X-Ricochet": "1" },
    });
    const texte = await reponse.text();
    if (messageOuvert?.id !== id) return;
    if (!reponse.ok) {
      etat.textContent = texte || `erreur ${reponse.status}`;
      $("#recharger-detail").hidden = false;
      return;
    }
    etat.textContent = "Détail lu sur la machine (non enregistré par le relais).";
    contenu.textContent = texte;
    contenu.hidden = false;
    $("#copier-detail").hidden = false;
  } catch {
    etat.textContent = "Relais injoignable.";
    $("#recharger-detail").hidden = false;
  }
}

async function copierDetail() {
  const bouton = $("#copier-detail");
  try {
    await navigator.clipboard.writeText($("#detail-contenu").textContent);
    bouton.textContent = "Copié";
  } catch {
    // Pas d'accès au presse-papiers : sélection pour copier à la main.
    const plage = document.createRange();
    plage.selectNodeContents($("#detail-contenu"));
    getSelection().removeAllRanges();
    getSelection().addRange(plage);
    bouton.textContent = "Sélectionné : copiez";
  }
  setTimeout(() => { bouton.textContent = "Copier"; }, 2000);
}

async function repondreMessage(m, bouton) {
  for (const b of document.querySelectorAll("#detail-boutons button")) b.disabled = true;
  try {
    await api("POST", `/messages/${encodeURIComponent(m.id)}/reponse`, { bouton });
    message("");
    await ouvrirMessage(m.id);
  } catch (erreur) {
    message(`Réponse : ${erreur.message}`, true);
    await ouvrirMessage(m.id);
  }
}

// --- Notifications Web Push ---

function pushPossible() {
  return "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;
}

function octetsDeBase64Url(texte) {
  const base64 = texte.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(texte.length / 4) * 4, "=");
  return Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
}

async function empreinteSha256(texte) {
  const resume = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(texte));
  return [...new Uint8Array(resume)].map((o) => o.toString(16).padStart(2, "0")).join("");
}

function nomAppareil() {
  const ua = navigator.userAgent;
  const systeme = /iPhone|iPad/.test(ua) ? "iPhone" : /Android/.test(ua) ? "Android"
    : /Windows/.test(ua) ? "Windows" : /Mac/.test(ua) ? "Mac" : /Linux/.test(ua) ? "Linux" : "appareil";
  const navigateur = /Edg\//.test(ua) ? "Edge" : /Firefox\//.test(ua) ? "Firefox"
    : /Chrome\//.test(ua) ? "Chrome" : /Safari\//.test(ua) ? "Safari" : "navigateur";
  return `${navigateur} sous ${systeme}`;
}

async function abonnementLocal() {
  if (!pushPossible()) return null;
  try {
    const registration = await navigator.serviceWorker.ready;
    return await registration.pushManager.getSubscription();
  } catch {
    return null;
  }
}

async function majNotifications() {
  const etat = $("#etat-notifications");
  const activer = $("#activer-notifications");
  const liste = $("#appareils");
  let local = null;
  let texte;
  if (!pushPossible()) {
    texte = /iPhone|iPad/.test(navigator.userAgent)
      ? "Sur iPhone (iOS 16.4 ou plus) : Partager, « Sur l'écran d'accueil », puis ouvrez " +
        "Ricochet depuis son icône pour activer les notifications."
      : "Ce navigateur ne sait pas recevoir de notifications.";
    activer.disabled = true;
  } else if (Notification.permission === "denied") {
    texte = "Les notifications sont bloquées pour ce site : les autoriser dans les réglages du navigateur.";
    activer.disabled = true;
  } else {
    local = await abonnementLocal();
    activer.disabled = Boolean(local);
    texte = local ? "Les notifications sont actives sur cet appareil."
      : "Les notifications ne sont pas activées sur cet appareil.";
  }
  etat.textContent = texte;
  $("#essai-notification").disabled = false;
  try {
    const appareils = await api("GET", "/push/abonnements");
    const mienne = local ? await empreinteSha256(local.endpoint) : null;
    liste.replaceChildren(...appareils.map((a) => {
      const li = element("li");
      const nom = element("span", "nom-appareil",
        `${a.appareil}${a.empreinte === mienne ? " (cet appareil)" : ""} · ${a.compte} · ${date(a.cree_le)}`);
      const retirer = element("button", "discret danger", "Retirer");
      retirer.type = "button";
      retirer.addEventListener("click", () => retirerAppareil(a, a.empreinte === mienne ? local : null));
      li.append(nom, retirer);
      return li;
    }));
  } catch (erreur) {
    if (erreur.statut !== 401) etat.textContent = `${texte} (liste des appareils : ${erreur.message})`;
  }
}

// Le geste de l'utilisateur (ce clic) est exigé par iOS pour demander la permission.
async function activerNotifications() {
  const bouton = $("#activer-notifications");
  bouton.disabled = true;
  try {
    const permission = await Notification.requestPermission();
    if (permission !== "granted") {
      message("Notifications refusées : les autoriser dans les réglages du navigateur.", true);
      return;
    }
    let cle;
    try {
      cle = (await api("GET", "/push/cle")).cle;
    } catch (erreur) {
      message(erreur.statut === 503 ? erreur.message : `Clé des notifications : ${erreur.message}`, true);
      return;
    }
    const registration = await navigator.serviceWorker.ready;
    const abonnement = await registration.pushManager.subscribe({
      userVisibleOnly: true,
      applicationServerKey: octetsDeBase64Url(cle),
    });
    try {
      await api("POST", "/push/abonnements", { ...abonnement.toJSON(), appareil: nomAppareil() });
    } catch (erreur) {
      await abonnement.unsubscribe();
      throw erreur;
    }
    message("Notifications activées sur cet appareil.");
  } catch (erreur) {
    message(`Notifications : ${erreur.message}`, true);
  } finally {
    await majNotifications();
  }
}

async function retirerAppareil(appareil, local) {
  if (!confirm(`Retirer « ${appareil.appareil} » des notifications ?`)) return;
  try {
    await api("DELETE", `/push/abonnements/${appareil.id}`);
    if (local) await local.unsubscribe();
  } catch (erreur) {
    message(`Retrait : ${erreur.message}`, true);
  }
  await majNotifications();
}

async function essaiNotification() {
  const bouton = $("#essai-notification");
  bouton.disabled = true;
  try {
    const bilan = await api("POST", "/push/essai");
    message(bilan.appareils === 0
      ? "Aucun appareil abonné : activez d'abord les notifications."
      : `Essai : ${bilan.envoyes} envoyée(s), ${bilan.echecs} échec(s), ${bilan.supprimes} appareil(s) retiré(s).`,
    bilan.envoyes === 0);
  } catch (erreur) {
    message(`Essai : ${erreur.message}`, true);
  } finally {
    bouton.disabled = false;
    majNotifications();
  }
}

function demarrerServiceWorker() {
  if (!("serviceWorker" in navigator)) return;
  navigator.serviceWorker.register("/sw.js").catch(() => {});
  // Toucher une notification alors que l'interface est ouverte : le service worker nous écrit.
  navigator.serviceWorker.addEventListener("message", (e) => {
    if (e.data?.type === "ouvrir-message" && minuterie) ouvrirDepuisAdresse(e.data.id);
  });
}

const IDENTIFIANT_MESSAGE = /^[A-Za-z0-9_-]{1,64}$/;

function ouvrirDepuisAdresse(id) {
  if (id && IDENTIFIANT_MESSAGE.test(id)) {
    ouvrirMessage(id);
  } else {
    changerOnglet("messages");
  }
}

function surChangementAdresse() {
  const reste = location.hash.startsWith("#message/") ? location.hash.slice("#message/".length) : null;
  if (reste !== null && minuterie) {
    ouvrirDepuisAdresse(decodeURIComponent(reste));
  }
}

function changerOnglet(nom, ouvrirListe = true) {
  for (const bouton of document.querySelectorAll("[data-onglet]")) {
    bouton.classList.toggle("actif", bouton.dataset.onglet === nom);
  }
  $("#onglet-machines").hidden = nom !== "machines";
  $("#onglet-messages").hidden = nom !== "messages";
  $("#onglet-journal").hidden = nom !== "journal";
  if (nom === "journal") chargerJournal();
  else if (nom === "messages") {
    if (ouvrirListe) {
      messageOuvert = null;
      $("#vue-detail-message").hidden = true;
      $("#vue-liste-messages").hidden = false;
      chargerMessages();
    }
    majNotifications();
  } else chargerMachines();
}

// --- Connexion ---

async function seConnecter(evenement) {
  evenement.preventDefault();
  const f = evenement.target;
  const bouton = f.querySelector("button");
  bouton.disabled = true;
  try {
    const reponse = await api("POST", "/connexion", {
      nom: f.nom.value.trim(),
      mot_de_passe: f.mot_de_passe.value,
      code: f.code.value.trim(),
    });
    f.reset();
    message("");
    afficherTableau(reponse.compte);
  } catch (erreur) {
    f.code.value = "";
    message(erreur.statut === 401 ? "Compte, mot de passe ou code refusé." : erreur.message, true);
  } finally {
    bouton.disabled = false;
  }
}

async function seDeconnecter() {
  let parPortier = false;
  try {
    parPortier = Boolean((await api("POST", "/deconnexion"))?.portier);
  } catch {
    // Session déjà expirée : rien à faire.
  }
  $("#machines").replaceChildren();
  $("#journal").replaceChildren();
  fermerJeton();
  fermerTerminal();
  fermerFichiers();
  if (portier && parPortier) {
    // Session ouverte par Portier : la session de Portier reste ouverte tant qu'on ne s'y déconnecte pas.
    window.location.assign(portier.deconnexion);
    return;
  }
  afficherConnexion();
}

async function chargerPortier() {
  try {
    const infos = await api("GET", "/portier");
    if (infos && infos.actif) {
      portier = { connexion: infos.connexion, deconnexion: infos.deconnexion };
      $("#lien-portier").href = portier.connexion;
      $("#portier").hidden = false;
    }
  } catch {
    // Relais plus ancien ou erreur : connexion habituelle seulement.
  }
}

async function demarrer() {
  $("#formulaire-connexion").addEventListener("submit", seConnecter);
  $("#formulaire-ajout").addEventListener("submit", ajouterMachine);
  $("#deconnexion").addEventListener("click", seDeconnecter);
  $("#copier").addEventListener("click", copierConfig);
  $("#retour-messages").addEventListener("click", fermerMessage);
  $("#copier-detail").addEventListener("click", copierDetail);
  $("#recharger-detail").addEventListener("click", () => messageOuvert && chargerDetail(messageOuvert.id));
  $("#activer-notifications").addEventListener("click", activerNotifications);
  $("#essai-notification").addEventListener("click", essaiNotification);
  window.addEventListener("hashchange", surChangementAdresse);
  demarrerServiceWorker();
  $("#fermer-jeton").addEventListener("click", fermerJeton);
  $("#fermer-terminal").addEventListener("click", fermerTerminal);
  $("#fermer-fichiers").addEventListener("click", fermerFichiers);
  $("#parent-fichiers").addEventListener("click", () => {
    if (fichiers) chargerDossier(parent(fichiers.chemin));
  });
  $("#aller-fichiers").addEventListener("submit", (e) => {
    e.preventDefault();
    chargerDossier($("#chemin-fichiers").value.trim());
  });
  $("#envoyer-fichiers").addEventListener("click", () => $("#choix-fichiers").click());
  $("#choix-fichiers").addEventListener("change", (e) => {
    const choisis = [...e.target.files];
    e.target.value = "";
    envoyerFichiers(choisis);
  });
  for (const bouton of document.querySelectorAll("[data-touche]")) {
    // Garder le focus (et le clavier du téléphone) sur le terminal.
    bouton.addEventListener("mousedown", (e) => e.preventDefault());
    bouton.addEventListener("click", () => toucheEcran(bouton.dataset.touche));
  }
  window.visualViewport?.addEventListener("resize", () => {
    // Le clavier du téléphone réduit la zone visible : le terminal s'y adapte.
    $("#ecran-terminal").style.height = `${window.visualViewport.height}px`;
    terminal?.ajustement.fit();
  });
  for (const bouton of document.querySelectorAll("[data-onglet]")) {
    bouton.addEventListener("click", () => changerOnglet(bouton.dataset.onglet));
  }
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden && minuterie && !$("#onglet-machines").hidden) chargerMachines();
    else if (!document.hidden && minuterie) majPastille();
  });
  await chargerPortier();
  try {
    const moi = await api("GET", "/moi");
    afficherTableau(moi.compte);
  } catch {
    afficherConnexion();
  }
}

demarrer();
