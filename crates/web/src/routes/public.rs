use askama::Template;
use axum::response::{Html, IntoResponse};

use crate::error::WebError;

#[derive(Template)]
#[template(path = "public/splash.html")]
pub struct SplashTemplate;

pub async fn splash() -> Result<impl IntoResponse, WebError> {
    Ok(Html(SplashTemplate.render()?))
}

#[derive(Template)]
#[template(path = "public/pricing.html")]
pub struct PricingTemplate;

pub async fn pricing() -> Result<impl IntoResponse, WebError> {
    Ok(Html(PricingTemplate.render()?))
}

#[derive(Template)]
#[template(path = "public/about.html")]
pub struct AboutTemplate;

pub async fn about() -> Result<impl IntoResponse, WebError> {
    Ok(Html(AboutTemplate.render()?))
}

#[derive(Template)]
#[template(path = "public/docs.html")]
pub struct DocsTemplate;

pub async fn docs() -> Result<impl IntoResponse, WebError> {
    Ok(Html(DocsTemplate.render()?))
}

#[derive(Template)]
#[template(path = "public/help.html")]
pub struct HelpTemplate;

pub async fn help() -> Result<impl IntoResponse, WebError> {
    Ok(Html(HelpTemplate.render()?))
}
