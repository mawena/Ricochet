//! API de l'opérateur.
//!
//! Accès par la session de l'interface (cookie) ou, pour les scripts, par le jeton de la
//! configuration (`Authorization: Bearer <jeton>`). Avec le cookie, toute requête qui modifie
//! quelque chose doit porter l'en-tête `X-Ricochet` : un autre site ne peut pas l'ajouter sans
//! l'accord du relais, ce qui bloque les requêtes forgées (CSRF).

use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::{MatchedPath, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, delete, get, post},
};
use ricochet_protocole::RelaisVersAgent;
use serde::Deserialize;
use serde_json::json;

use crate::{
    Adresse, Relais, applis, comptes, config, fichiers, jeton, messages, portier, push,
    registre::{Commande, ErreurEnvoi, VueMachine, maintenant},
    terminal, totp, tunnel,
};

/// Nom du compte inscrit au journal pour les appels faits avec le jeton de la configuration.
const COMPTE_JETON: &str = "jeton-api";
/// Nom du compte inscrit au journal pour les appels faits avec le jeton des tunnels.
const COMPTE_TUNNEL: &str = "jeton-tunnel";
/// Les seules routes ouvertes au jeton des tunnels, en lecture.
const ROUTES_TUNNEL: [&str; 2] = ["/api/machines", "/api/machines/{id}/tunnel"];

/// L'opérateur authentifié d'une requête.
#[derive(Clone)]
pub(crate) struct Operateur {
    pub compte: String,
    /// Empreinte de la session (absente avec le jeton de la configuration).
    pub session: Option<String>,
}

/// Erreur interne : journalisée, renvoyée sans détail.
pub(crate) struct Erreur(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for Erreur {
    fn from(erreur: E) -> Self {
        Self(erreur.into())
    }
}

impl IntoResponse for Erreur {
    fn into_response(self) -> Response {
        tracing::error!("erreur interne : {:#}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, "erreur interne").into_response()
    }
}

type Resultat = Result<Response, Erreur>;

pub fn routeur(relais: Arc<Relais>) -> Router<Arc<Relais>> {
    let protegees = Router::new()
        .route("/deconnexion", post(deconnexion))
        .route("/moi", get(moi))
        .route("/machines", get(machines).post(ajouter_machine))
        .route("/machines/{id}", delete(revoquer_machine))
        .route("/machines/{id}/actualiser", post(actualiser))
        .route("/machines/{id}/terminal", get(terminal::connexion))
        .route("/machines/{id}/tunnel", get(tunnel::connexion))
        .route(
            "/machines/{id}/fichiers",
            get(fichiers::lister).put(fichiers::envoyer),
        )
        .route("/machines/{id}/fichiers/partiel", get(fichiers::partiel))
        .route(
            "/machines/{id}/fichiers/telecharger",
            get(fichiers::telecharger),
        )
        .route("/messages", get(messages::liste))
        .route("/messages/{id}", get(messages::un))
        .route("/messages/{id}/detail", get(messages::detail))
        .route("/messages/{id}/reponse", post(messages::repondre))
        .route("/push/cle", get(push::cle))
        .route("/push/abonnements", get(push::liste).post(push::ajouter))
        .route("/push/abonnements/{id}", delete(push::retirer))
        .route("/push/essai", post(push::essai))
        .route("/audit", get(audit))
        .layer(middleware::from_fn_with_state(relais.clone(), authentifier));
    // Applications web : leurs pages ne portent pas `X-Ricochet`, la vérification de l'origine
    // remplace `contre_csrf` (voir `applis.rs`).
    let applis = Router::new()
        .route("/machines/{id}/applis/{nom}", any(applis::racine))
        .route("/machines/{id}/applis/{nom}/", any(applis::requete))
        .route(
            "/machines/{id}/applis/{nom}/{*chemin}",
            any(applis::requete),
        )
        .layer(middleware::from_fn(applis::origine_exigee))
        .layer(middleware::from_fn_with_state(relais, authentifier));
    Router::new()
        .route("/connexion", post(connexion))
        .route("/portier", get(infos_portier))
        .merge(protegees)
        .layer(middleware::from_fn(contre_csrf))
        .merge(applis)
}

/// Nom du cookie de session : le préfixe `__Host-` l'attache au seul domaine du relais, en HTTPS.
fn nom_cookie(relais: &Relais) -> &'static str {
    if relais.config.cookie_securise {
        "__Host-ricochet"
    } else {
        "ricochet"
    }
}

/// Cookie de session à envoyer au navigateur.
pub(crate) fn cookie_session(relais: &Relais, valeur: &str, duree_s: u64) -> String {
    let securise = if relais.config.cookie_securise {
        "; Secure"
    } else {
        ""
    };
    format!(
        "{}={valeur}; Path=/; HttpOnly; SameSite=Strict; Max-Age={duree_s}{securise}",
        nom_cookie(relais)
    )
}

pub(crate) fn cookie_recu(relais: &Relais, entetes: &HeaderMap) -> Option<String> {
    let nom = nom_cookie(relais);
    entetes
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|paire| paire.trim().split_once('='))
        .find(|(n, _)| *n == nom)
        .map(|(_, valeur)| valeur.to_owned())
}

fn bearer(entetes: &HeaderMap) -> Option<&str> {
    entetes
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

/// Refuse les requêtes qui modifient quelque chose sans l'en-tête `X-Ricochet`, sauf avec le
/// jeton de la configuration (qu'un navigateur n'envoie jamais de lui-même).
async fn contre_csrf(requete: Request, suite: Next) -> Response {
    let lecture = matches!(*requete.method(), Method::GET | Method::HEAD);
    if !lecture
        && bearer(requete.headers()).is_none()
        && !requete.headers().contains_key("x-ricochet")
    {
        return (StatusCode::FORBIDDEN, "en-tête X-Ricochet manquant").into_response();
    }
    suite.run(requete).await
}

async fn authentifier(
    State(relais): State<Arc<Relais>>,
    mut requete: Request,
    suite: Next,
) -> Response {
    let operateur = if let Some(recu) = bearer(requete.headers()) {
        let correspond = |empreinte: &Option<String>| {
            empreinte
                .as_ref()
                .is_some_and(|empreinte| jeton::verifier(recu, empreinte))
        };
        if correspond(&relais.config.jeton_operateur_sha256) {
            Some(Operateur {
                compte: COMPTE_JETON.into(),
                session: None,
            })
        } else if correspond(&relais.config.jeton_tunnel_sha256) {
            // Ce jeton n'ouvre que la liste des machines et les tunnels.
            let route = requete
                .extensions()
                .get::<MatchedPath>()
                .map(|r| r.as_str());
            if requete.method() != Method::GET || !route.is_some_and(|r| ROUTES_TUNNEL.contains(&r))
            {
                return (StatusCode::FORBIDDEN, "ce jeton n'ouvre que les tunnels").into_response();
            }
            Some(Operateur {
                compte: COMPTE_TUNNEL.into(),
                session: None,
            })
        } else {
            None
        }
    } else if let Some(valeur) = cookie_recu(&relais, requete.headers()) {
        let empreinte = jeton::empreinte(&valeur);
        let instant = maintenant();
        match relais.base.session(&empreinte, instant) {
            Ok(compte) => compte
                .or_else(|| relais.sessions_portier.compte(&empreinte, instant))
                .map(|compte| Operateur {
                    compte,
                    session: Some(empreinte),
                }),
            Err(erreur) => return Erreur(erreur).into_response(),
        }
    } else {
        None
    };
    match operateur {
        Some(operateur) => {
            requete.extensions_mut().insert(operateur);
            suite.run(requete).await
        }
        None => (StatusCode::UNAUTHORIZED, "connexion requise").into_response(),
    }
}

#[derive(Deserialize)]
struct Identifiants {
    nom: String,
    mot_de_passe: String,
    code: String,
}

/// Connexion : mot de passe et code TOTP ensemble. La réponse ne dit jamais lequel est faux.
async fn connexion(
    State(relais): State<Arc<Relais>>,
    Adresse(adresse): Adresse,
    Json(identifiants): Json<Identifiants>,
) -> Resultat {
    let instant = maintenant();
    let cle_adresse = format!("adresse:{adresse}");
    let cle_compte = format!("compte:{}", identifiants.nom);
    if relais.essais_adresse.bloque(&cle_adresse, instant)
        || relais.essais_compte.bloque(&cle_compte, instant)
    {
        relais.noter(None, &adresse, "connexion_bloquee", &identifiants.nom);
        let minutes = comptes::FENETRE_S / 60;
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            format!("trop d'essais, réessayez dans {minutes} minutes"),
        )
            .into_response());
    }

    let compte = relais.base.compte(&identifiants.nom)?;
    let empreinte = compte.as_ref().map(|c| c.mot_de_passe.clone());
    let mot_de_passe = identifiants.mot_de_passe;
    let mot_de_passe_bon =
        tokio::task::spawn_blocking(move || comptes::verifier(&mot_de_passe, empreinte.as_deref()))
            .await?;
    let accepte = match &compte {
        Some(compte) if mot_de_passe_bon => {
            match totp::verifier(
                &compte.totp_secret,
                &identifiants.code,
                instant,
                compte.totp_dernier_pas,
            ) {
                Some(pas) => relais.base.noter_pas_totp(&compte.nom, pas)?,
                None => false,
            }
        }
        _ => false,
    };
    if !accepte {
        relais.essais_adresse.echec(&cle_adresse, instant);
        relais.essais_compte.echec(&cle_compte, instant);
        relais.noter(None, &adresse, "connexion_refusee", &identifiants.nom);
        return Ok((StatusCode::UNAUTHORIZED, "identifiants refusés").into_response());
    }

    relais.essais_compte.oublier(&cle_compte);
    let valeur = jeton::nouveau()?;
    let duree_s = relais.config.duree_session_h * 3600;
    relais.base.creer_session(
        &jeton::empreinte(&valeur),
        &identifiants.nom,
        instant,
        instant + duree_s,
        &adresse,
    )?;
    relais.noter(Some(&identifiants.nom), &adresse, "connexion", "");
    let cookie = cookie_session(&relais, &valeur, duree_s);
    Ok((
        [(header::SET_COOKIE, HeaderValue::from_str(&cookie)?)],
        Json(json!({ "compte": identifiants.nom })),
    )
        .into_response())
}

async fn deconnexion(
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
) -> Resultat {
    let mut par_portier = false;
    if let Some(session) = &operateur.session {
        relais.base.supprimer_session(session)?;
        par_portier = relais.sessions_portier.supprimer(session);
    }
    relais.noter(Some(&operateur.compte), &adresse, "deconnexion", "");
    let cookie = format!("{}=; Path=/; Max-Age=0", nom_cookie(&relais));
    // `portier` : la session venait de Portier, l'interface renvoie alors vers sa déconnexion
    // (sinon la page suivante en rouvrirait une). Connexion par mot de passe : rien de plus.
    Ok((
        [(header::SET_COOKIE, HeaderValue::from_str(&cookie)?)],
        Json(json!({ "portier": par_portier })),
    )
        .into_response())
}

/// Mode Portier, pour la page de connexion : adresse du bouton « Se connecter via Portier »
/// (retour vers l'interface telle que demandée) et adresse où renvoie la déconnexion.
async fn infos_portier(
    State(relais): State<Arc<Relais>>,
    entetes: HeaderMap,
) -> Json<serde_json::Value> {
    let reglage = &relais.config.portier;
    if !reglage.actif {
        return Json(json!({ "actif": false }));
    }
    let retour = portier::adresse_interface(&entetes, relais.config.cookie_securise);
    Json(json!({
        "actif": true,
        "connexion": portier::adresse_connexion(reglage, &retour),
        "deconnexion": reglage.adresse_deconnexion(),
    }))
}

async fn moi(Extension(operateur): Extension<Operateur>) -> Json<serde_json::Value> {
    Json(json!({ "compte": operateur.compte }))
}

async fn machines(State(relais): State<Arc<Relais>>) -> Result<Json<Vec<VueMachine>>, Erreur> {
    Ok(Json(relais.registre.liste(&relais.base.machines()?)))
}

#[derive(Deserialize)]
struct NouvelleMachine {
    id: String,
    #[serde(default)]
    nom: Option<String>,
}

/// Déclare une machine et renvoie son jeton : il n'est montré qu'une fois, seule son empreinte
/// est gardée.
async fn ajouter_machine(
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
    Json(nouvelle): Json<NouvelleMachine>,
) -> Resultat {
    if !config::identifiant_valide(&nouvelle.id) {
        return Ok((
            StatusCode::BAD_REQUEST,
            "identifiant invalide : lettres, chiffres, - et _ (64 au plus)",
        )
            .into_response());
    }
    let nom = nouvelle
        .nom
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    if nom.is_some_and(|n| n.chars().count() > 100) {
        return Ok((StatusCode::BAD_REQUEST, "nom trop long (100 au plus)").into_response());
    }
    let valeur = jeton::nouveau()?;
    if !relais
        .base
        .ajouter_machine(&nouvelle.id, nom, &jeton::empreinte(&valeur), maintenant())?
    {
        return Ok((StatusCode::CONFLICT, "cet identifiant existe déjà").into_response());
    }
    relais.noter(
        Some(&operateur.compte),
        &adresse,
        "machine_ajoutee",
        &nouvelle.id,
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": nouvelle.id, "jeton": valeur })),
    )
        .into_response())
}

/// Révoque une machine : son jeton ne vaut plus rien et sa connexion est fermée.
async fn revoquer_machine(
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
    Path(id): Path<String>,
) -> Resultat {
    if !relais.base.supprimer_machine(&id)? {
        return Ok((StatusCode::NOT_FOUND, "machine inconnue").into_response());
    }
    relais.registre.retirer(&id);
    relais.noter(Some(&operateur.compte), &adresse, "machine_revoquee", &id);
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Demande à une machine connectée d'envoyer son état sans attendre.
async fn actualiser(
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
    Path(id): Path<String>,
) -> Resultat {
    if relais.base.empreinte_machine(&id)?.is_none() {
        return Ok((StatusCode::NOT_FOUND, "machine inconnue").into_response());
    }
    relais.noter(Some(&operateur.compte), &adresse, "etat_demande", &id);
    Ok(
        match relais
            .registre
            .envoyer(&id, Commande::Envoyer(RelaisVersAgent::DemandeEtat))
        {
            Ok(()) => StatusCode::ACCEPTED.into_response(),
            Err(ErreurEnvoi::Deconnectee) => {
                (StatusCode::CONFLICT, "machine déconnectée").into_response()
            }
            Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "machine occupée").into_response(),
        },
    )
}

#[derive(Deserialize)]
struct ParametresAudit {
    limite: Option<u32>,
}

async fn audit(
    State(relais): State<Arc<Relais>>,
    Query(parametres): Query<ParametresAudit>,
) -> Resultat {
    let limite = parametres.limite.unwrap_or(200).clamp(1, 1000);
    Ok(Json(relais.base.journal(limite)?).into_response())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use tower::ServiceExt;

    use crate::{Base, Config, Relais, application, comptes, jeton, totp};

    const SECRET: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    fn relais() -> Arc<Relais> {
        let config = Config::depuis_texte(&format!(
            "jeton_operateur_sha256 = \"{}\"\ncookie_securise = false\n",
            jeton::empreinte("operateur"),
        ))
        .unwrap();
        let base = Base::en_memoire();
        let empreinte = comptes::hacher("mot de passe solide").unwrap();
        base.creer_compte("charles", &empreinte, SECRET, 0).unwrap();
        base.ajouter_machine("pc", Some("Mon PC"), &jeton::empreinte("agent"), 0)
            .unwrap();
        Arc::new(Relais::new(config, base))
    }

    /// Code TOTP valable maintenant pour `SECRET`.
    fn code_actuel() -> String {
        totp::code_pour(SECRET, crate::registre::maintenant())
    }

    struct Reponse {
        statut: u16,
        cookie: Option<String>,
        corps: String,
    }

    async fn appeler(
        relais: &Arc<Relais>,
        methode: &str,
        chemin: &str,
        entetes: &[(&str, &str)],
        corps: Option<serde_json::Value>,
    ) -> Reponse {
        let mut requete = axum::http::Request::builder().method(methode).uri(chemin);
        for (nom, valeur) in entetes {
            requete = requete.header(*nom, *valeur);
        }
        let corps = match corps {
            Some(valeur) => {
                requete = requete.header("content-type", "application/json");
                Body::from(valeur.to_string())
            }
            None => Body::empty(),
        };
        let reponse = application(relais.clone())
            .oneshot(requete.body(corps).unwrap())
            .await
            .unwrap();
        let statut = reponse.status().as_u16();
        let cookie = reponse
            .headers()
            .get("set-cookie")
            .map(|v| v.to_str().unwrap().to_owned());
        let corps = to_bytes(reponse.into_body(), 1 << 20).await.unwrap();
        Reponse {
            statut,
            cookie,
            corps: String::from_utf8(corps.to_vec()).unwrap(),
        }
    }

    const BEARER: (&str, &str) = ("authorization", "Bearer operateur");

    async fn se_connecter(relais: &Arc<Relais>, mot_de_passe: &str, code: &str) -> Reponse {
        appeler(
            relais,
            "POST",
            "/api/connexion",
            &[("x-ricochet", "1")],
            Some(serde_json::json!({"nom": "charles", "mot_de_passe": mot_de_passe, "code": code})),
        )
        .await
    }

    #[tokio::test]
    async fn sante_et_interface_sans_connexion() {
        let relais = relais();
        assert_eq!(
            appeler(&relais, "GET", "/sante", &[], None).await.corps,
            "ok"
        );
        let accueil = appeler(&relais, "GET", "/", &[], None).await;
        assert_eq!(accueil.statut, 200);
        assert!(accueil.corps.contains("Ricochet"));
    }

    #[tokio::test]
    async fn authentification_obligatoire() {
        let relais = relais();
        assert_eq!(
            appeler(&relais, "GET", "/api/machines", &[], None)
                .await
                .statut,
            401
        );
        let faux = [("authorization", "Bearer faux")];
        assert_eq!(
            appeler(&relais, "GET", "/api/machines", &faux, None)
                .await
                .statut,
            401
        );
        // Le jeton d'un agent n'ouvre pas l'API.
        let agent = [("authorization", "Bearer agent")];
        assert_eq!(
            appeler(&relais, "GET", "/api/machines", &agent, None)
                .await
                .statut,
            401
        );
        let cookie = [("cookie", "ricochet=inconnu")];
        assert_eq!(
            appeler(&relais, "GET", "/api/machines", &cookie, None)
                .await
                .statut,
            401
        );
    }

    #[tokio::test]
    async fn messages_et_push_reserves_a_l_operateur() {
        let relais = relais();
        for (methode, chemin) in [
            ("GET", "/api/messages"),
            ("GET", "/api/messages/x"),
            ("GET", "/api/messages/x/detail"),
            ("POST", "/api/messages/x/reponse"),
            ("GET", "/api/push/cle"),
            ("GET", "/api/push/abonnements"),
            ("POST", "/api/push/abonnements"),
            ("DELETE", "/api/push/abonnements/1"),
            ("POST", "/api/push/essai"),
        ] {
            let sans = appeler(&relais, methode, chemin, &[("x-ricochet", "1")], None).await;
            assert_eq!(sans.statut, 401, "{methode} {chemin}");
        }
        // Avec le jeton opérateur : la liste est vide, la clé demande la configuration du relais.
        let liste = appeler(&relais, "GET", "/api/messages", &[BEARER], None).await;
        assert_eq!(liste.statut, 200);
        assert!(liste.corps.contains("\"non_lus\":0"));
        assert_eq!(
            appeler(&relais, "GET", "/api/messages/x", &[BEARER], None)
                .await
                .statut,
            404
        );
        assert_eq!(
            appeler(&relais, "GET", "/api/push/cle", &[BEARER], None)
                .await
                .statut,
            503
        );
        assert_eq!(
            appeler(&relais, "POST", "/api/push/essai", &[BEARER], None)
                .await
                .statut,
            503
        );
    }

    #[tokio::test]
    async fn interface_installable() {
        let relais = relais();
        let sw = appeler(&relais, "GET", "/sw.js", &[], None).await;
        assert_eq!(sw.statut, 200);
        assert!(sw.corps.contains("notificationclick") && sw.corps.contains("showNotification"));
        let manifeste = appeler(&relais, "GET", "/manifest.json", &[], None).await;
        let m: serde_json::Value = serde_json::from_str(&manifeste.corps).unwrap();
        assert_eq!(m["display"], "standalone");
        let tailles: Vec<_> = m["icons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["sizes"].as_str().unwrap().to_owned())
            .collect();
        assert!(tailles.contains(&"192x192".to_owned()) && tailles.contains(&"512x512".to_owned()));
        let accueil = appeler(&relais, "GET", "/", &[], None).await;
        assert!(accueil.corps.contains("apple-touch-icon"));
        // Les icônes sont de vrais PNG aux bonnes dimensions.
        for (chemin, taille) in [
            ("/icone-192.png", 192u32),
            ("/icone-512.png", 512),
            ("/apple-touch-icon.png", 180),
        ] {
            let requete = axum::http::Request::builder()
                .uri(chemin)
                .body(Body::empty())
                .unwrap();
            let reponse = application(relais.clone()).oneshot(requete).await.unwrap();
            assert_eq!(reponse.status(), 200, "{chemin}");
            assert_eq!(reponse.headers()["content-type"], "image/png");
            let csp = reponse.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .to_owned();
            assert!(csp.contains("worker-src 'self'") && csp.contains("manifest-src 'self'"));
            assert!(!csp.contains("unsafe-eval") && !csp.contains("script-src 'self' 'unsafe"));
            let octets = to_bytes(reponse.into_body(), 1 << 20).await.unwrap();
            assert_eq!(&octets[..8], b"\x89PNG\r\n\x1a\n", "{chemin}");
            let largeur = u32::from_be_bytes(octets[16..20].try_into().unwrap());
            let hauteur = u32::from_be_bytes(octets[20..24].try_into().unwrap());
            assert_eq!((largeur, hauteur), (taille, taille), "{chemin}");
        }
        // Aucune donnée reçue ne passe par innerHTML.
        let script = appeler(&relais, "GET", "/app.js", &[], None).await;
        let code: String = script
            .corps
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect();
        assert!(!code.contains("innerHTML") && !code.contains("insertAdjacentHTML"));
        assert!(!sw.corps.contains("innerHTML"));
    }

    #[tokio::test]
    async fn connexion_complete() {
        let relais = relais();
        assert_eq!(
            se_connecter(&relais, "mot de passe solide", "000000")
                .await
                .statut,
            401
        );
        let code = code_actuel();
        assert_eq!(se_connecter(&relais, "mauvais", &code).await.statut, 401);

        let reponse = se_connecter(&relais, "mot de passe solide", &code).await;
        assert_eq!(reponse.statut, 200, "{}", reponse.corps);
        let cookie = reponse.cookie.unwrap();
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
        let valeur = cookie.split(';').next().unwrap().to_owned();

        // Le même code ne sert pas deux fois.
        assert_eq!(
            se_connecter(&relais, "mot de passe solide", &code)
                .await
                .statut,
            401
        );

        let session = [("cookie", valeur.as_str())];
        let moi = appeler(&relais, "GET", "/api/moi", &session, None).await;
        assert_eq!(moi.statut, 200);
        assert!(moi.corps.contains("charles"));

        // Sans l'en-tête X-Ricochet, une requête qui modifie est refusée (CSRF).
        let sortie = appeler(&relais, "POST", "/api/deconnexion", &session, None).await;
        assert_eq!(sortie.statut, 403);
        let session_csrf = [("cookie", valeur.as_str()), ("x-ricochet", "1")];
        let sortie = appeler(&relais, "POST", "/api/deconnexion", &session_csrf, None).await;
        assert_eq!(sortie.statut, 200);
        assert!(sortie.corps.contains("\"portier\":false"));
        assert_eq!(
            appeler(&relais, "GET", "/api/moi", &session, None)
                .await
                .statut,
            401
        );

        let actions: Vec<String> = relais
            .base
            .journal(10)
            .unwrap()
            .into_iter()
            .map(|l| l.action)
            .collect();
        assert_eq!(
            actions,
            [
                "deconnexion",
                "connexion_refusee",
                "connexion",
                "connexion_refusee",
                "connexion_refusee"
            ]
        );
    }

    #[tokio::test]
    async fn trop_d_essais() {
        let relais = relais();
        for _ in 0..comptes::ECHECS_MAX_ADRESSE {
            assert_eq!(se_connecter(&relais, "faux", "000000").await.statut, 401);
        }
        // Même le bon mot de passe est refusé tant que l'adresse est bloquée.
        let code = code_actuel();
        let reponse = se_connecter(&relais, "mot de passe solide", &code).await;
        assert_eq!(reponse.statut, 429);
    }

    #[tokio::test]
    async fn liste_des_machines() {
        let relais = relais();
        let reponse = appeler(&relais, "GET", "/api/machines", &[BEARER], None).await;
        assert_eq!(reponse.statut, 200);
        let liste: serde_json::Value = serde_json::from_str(&reponse.corps).unwrap();
        assert_eq!(liste[0]["id"], "pc");
        assert_eq!(liste[0]["nom"], "Mon PC");
        assert_eq!(liste[0]["connectee"], false);
    }

    #[tokio::test]
    async fn ajout_et_revocation() {
        let relais = relais();
        let ajout = |id: &str| serde_json::json!({ "id": id, "nom": "Serveur" });
        let reponse = appeler(
            &relais,
            "POST",
            "/api/machines",
            &[BEARER],
            Some(ajout("srv")),
        )
        .await;
        assert_eq!(reponse.statut, 201);
        let cree: serde_json::Value = serde_json::from_str(&reponse.corps).unwrap();
        let jeton_srv = cree["jeton"].as_str().unwrap();
        assert!(jeton::verifier(
            jeton_srv,
            &relais.base.empreinte_machine("srv").unwrap().unwrap()
        ));

        let doublon = appeler(
            &relais,
            "POST",
            "/api/machines",
            &[BEARER],
            Some(ajout("srv")),
        )
        .await;
        assert_eq!(doublon.statut, 409);
        let invalide = appeler(
            &relais,
            "POST",
            "/api/machines",
            &[BEARER],
            Some(ajout("a b")),
        )
        .await;
        assert_eq!(invalide.statut, 400);

        assert_eq!(
            appeler(&relais, "DELETE", "/api/machines/srv", &[BEARER], None)
                .await
                .statut,
            204
        );
        assert_eq!(
            appeler(&relais, "DELETE", "/api/machines/srv", &[BEARER], None)
                .await
                .statut,
            404
        );
        assert!(relais.base.empreinte_machine("srv").unwrap().is_none());

        let journal = appeler(&relais, "GET", "/api/audit?limite=5", &[BEARER], None).await;
        let lignes: serde_json::Value = serde_json::from_str(&journal.corps).unwrap();
        assert_eq!(lignes[0]["action"], "machine_revoquee");
        assert_eq!(lignes[0]["compte"], "jeton-api");
        assert_eq!(lignes[1]["action"], "machine_ajoutee");
    }

    #[tokio::test]
    async fn actualiser_machine() {
        let relais = relais();
        let inconnue = "/api/machines/inconnue/actualiser";
        assert_eq!(
            appeler(&relais, "POST", inconnue, &[BEARER], None)
                .await
                .statut,
            404
        );
        let pc = "/api/machines/pc/actualiser";
        assert_eq!(
            appeler(&relais, "POST", pc, &[BEARER], None).await.statut,
            409
        );
    }

    // --- Mode Portier ---

    const LOCAL: &str = "127.0.0.1:50000";
    const PORTIER: (&str, &str) = ("x-portier-utilisateur", "charles-gamligo");

    fn relais_portier() -> Arc<Relais> {
        let config = Config::depuis_texte(
            "cookie_securise = false\n[portier]\nactif = true\n\
             utilisateurs = [\"charles-gamligo\"]\nurl = \"https://portier.mawena.cloud\"\n",
        )
        .unwrap();
        let base = Base::en_memoire();
        base.ajouter_machine("pc", None, &jeton::empreinte("agent"), 0)
            .unwrap();
        Arc::new(Relais::new(config, base))
    }

    /// Requête venue de l'adresse `pair` (comme derrière nginx).
    async fn appeler_depuis(
        relais: &Arc<Relais>,
        pair: &str,
        chemin: &str,
        entetes: &[(&str, &str)],
    ) -> Reponse {
        let mut requete = axum::http::Request::get(chemin);
        for (nom, valeur) in entetes {
            requete = requete.header(*nom, *valeur);
        }
        let mut requete = requete.body(Body::empty()).unwrap();
        let pair: std::net::SocketAddr = pair.parse().unwrap();
        requete
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(pair));
        let reponse = application(relais.clone()).oneshot(requete).await.unwrap();
        let statut = reponse.status().as_u16();
        let cookie = reponse
            .headers()
            .get("set-cookie")
            .map(|v| v.to_str().unwrap().to_owned());
        let corps = to_bytes(reponse.into_body(), 1 << 20).await.unwrap();
        Reponse {
            statut,
            cookie,
            corps: String::from_utf8_lossy(&corps).into_owned(),
        }
    }

    #[tokio::test]
    async fn portier_entete_accepte() {
        let relais = relais_portier();
        let page = appeler_depuis(&relais, LOCAL, "/", &[PORTIER]).await;
        assert_eq!(page.statut, 200);
        let cookie = page.cookie.expect("session ouverte par Portier");
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
        let valeur = cookie.split(';').next().unwrap().to_owned();

        let session = [("cookie", valeur.as_str())];
        let moi = appeler(&relais, "GET", "/api/moi", &session, None).await;
        assert_eq!(moi.statut, 200);
        assert!(moi.corps.contains("charles-gamligo"));
        assert_eq!(
            appeler(&relais, "GET", "/api/machines", &session, None)
                .await
                .statut,
            200
        );
        // Une session existe déjà à ce nom : pas de nouvelle.
        let encore = appeler_depuis(&relais, LOCAL, "/", &[PORTIER, session[0]]).await;
        assert!(encore.cookie.is_none());
        // Même via [::1].
        assert!(
            appeler_depuis(&relais, "[::1]:50000", "/", &[PORTIER])
                .await
                .cookie
                .is_some()
        );

        let sortie = [("cookie", valeur.as_str()), ("x-ricochet", "1")];
        let sortie = appeler(&relais, "POST", "/api/deconnexion", &sortie, None).await;
        assert_eq!(sortie.statut, 200);
        assert!(sortie.corps.contains("\"portier\":true"));
        assert_eq!(
            appeler(&relais, "GET", "/api/moi", &session, None)
                .await
                .statut,
            401
        );
        let journal = relais.base.journal(10).unwrap();
        assert!(
            journal.iter().any(|l| l.action == "connexion_portier"
                && l.compte.as_deref() == Some("charles-gamligo"))
        );
    }

    #[tokio::test]
    async fn portier_nom_absent_de_la_liste() {
        let relais = relais_portier();
        let intrus = [("x-portier-utilisateur", "intrus")];
        let page = appeler_depuis(&relais, LOCAL, "/", &intrus).await;
        assert_eq!(page.statut, 200);
        assert!(page.cookie.is_none());
        // Deux en-têtes (même valides) : ignorés.
        let double = appeler_depuis(&relais, LOCAL, "/", &[PORTIER, PORTIER]).await;
        assert!(double.cookie.is_none());
    }

    #[tokio::test]
    async fn portier_ip_non_locale() {
        let relais = relais_portier();
        let page = appeler_depuis(&relais, "203.0.113.5:4000", "/", &[PORTIER]).await;
        assert_eq!(page.statut, 200);
        assert!(page.cookie.is_none());
    }

    #[tokio::test]
    async fn portier_option_desactivee() {
        let relais = relais();
        let page = appeler_depuis(&relais, LOCAL, "/", &[PORTIER]).await;
        assert_eq!(page.statut, 200);
        assert!(page.cookie.is_none());
        let infos = appeler(&relais, "GET", "/api/portier", &[], None).await;
        assert_eq!(infos.corps, r#"{"actif":false}"#);
    }

    #[tokio::test]
    async fn portier_ignore_sur_les_points_des_agents_et_clients() {
        let relais = relais_portier();
        for chemin in [
            "/api/moi",
            "/api/machines",
            "/api/machines/pc/tunnel?port=22",
            "/api/machines/pc/terminal",
            "/api/audit",
        ] {
            let reponse = appeler_depuis(&relais, LOCAL, chemin, &[PORTIER]).await;
            assert_eq!(reponse.statut, 401, "{chemin}");
            assert!(reponse.cookie.is_none(), "{chemin}");
        }
        for chemin in ["/agent", "/sante"] {
            let reponse = appeler_depuis(&relais, LOCAL, chemin, &[PORTIER]).await;
            assert!(reponse.cookie.is_none(), "{chemin}");
            assert_ne!(reponse.statut, 101, "{chemin}");
        }
        // Les fichiers de l'interface n'ouvrent pas de session non plus : seulement la page.
        assert!(
            appeler_depuis(&relais, LOCAL, "/app.js", &[PORTIER])
                .await
                .cookie
                .is_none()
        );
        assert!(relais.base.journal(10).unwrap().is_empty());
    }

    #[tokio::test]
    async fn portier_bouton_et_deconnexion() {
        let relais = relais_portier();
        let entetes = [
            ("host", "ricochet.mawena.cloud"),
            ("x-forwarded-proto", "https"),
        ];
        let infos = appeler(&relais, "GET", "/api/portier", &entetes, None).await;
        assert_eq!(infos.statut, 200);
        let infos: serde_json::Value = serde_json::from_str(&infos.corps).unwrap();
        assert_eq!(infos["actif"], true);
        assert_eq!(
            infos["connexion"],
            "https://portier.mawena.cloud/connexion?retour=https%3A%2F%2Fricochet.mawena.cloud%2F"
        );
        assert_eq!(infos["deconnexion"], "https://portier.mawena.cloud/");
    }

    #[tokio::test]
    async fn entetes_de_securite() {
        let relais = relais();
        let reponse = application(relais)
            .oneshot(axum::http::Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let csp = reponse.headers()["content-security-policy"]
            .to_str()
            .unwrap();
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(
            csp.contains("script-src 'self';"),
            "aucun script en ligne : {csp}"
        );
        assert_eq!(reponse.headers()["x-frame-options"], "DENY");
    }
}
