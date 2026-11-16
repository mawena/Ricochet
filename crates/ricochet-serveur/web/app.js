// Interface de Ricochet. Les données reçues ne passent jamais par innerHTML : seulement
// textContent, pour qu'un nom de machine ne puisse pas injecter de code.
"use strict";

const PERIODE_MS = 10000;
const $ = (selecteur) => document.querySelector(selecteur);
let minuterie = null;

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
  clearInterval(minuterie);
  minuterie = setInterval(() => {
    if (!document.hidden && !$("#onglet-machines").hidden && !terminal && !fichiers) {
      chargerMachines();
    }
  }, PERIODE_MS);
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
    `curl -fsSL https://mawena.github.io/Ricochet/installer.sh | sudo sh &&\n` +
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

function changerOnglet(nom) {
  for (const bouton of document.querySelectorAll("[data-onglet]")) {
    bouton.classList.toggle("actif", bouton.dataset.onglet === nom);
  }
  $("#onglet-machines").hidden = nom !== "machines";
  $("#onglet-journal").hidden = nom !== "journal";
  if (nom === "journal") chargerJournal();
  else chargerMachines();
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
  try {
    await api("POST", "/deconnexion");
  } catch {
    // Session déjà expirée : rien à faire.
  }
  $("#machines").replaceChildren();
  $("#journal").replaceChildren();
  fermerJeton();
  fermerTerminal();
  fermerFichiers();
  afficherConnexion();
}

async function demarrer() {
  $("#formulaire-connexion").addEventListener("submit", seConnecter);
  $("#formulaire-ajout").addEventListener("submit", ajouterMachine);
  $("#deconnexion").addEventListener("click", seDeconnecter);
  $("#copier").addEventListener("click", copierConfig);
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
  });
  try {
    const moi = await api("GET", "/moi");
    afficherTableau(moi.compte);
  } catch {
    afficherConnexion();
  }
}

demarrer();
