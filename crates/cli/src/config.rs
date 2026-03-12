// 'anyhow' is the industry standard crate for application-level error handling.
// It allows us to easily add context to errors and bubble them up.
use anyhow::{Context, Result, bail};
// 'serde' allows us to convert between Rust structs and data formats (like YAML/JSON).
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

// #[derive(...)] writes boilerplate code for us at compile time.
// Debug: Lets us print this struct to the console.
// Default: Lets us initialize a totally empty version of this struct.
// Deserialize/Serialize: Generates the code to turn this memory into YAML and back.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    // String in Rust is an OWNED, heap-allocated buffer of UTF-8 bytes.
    // Option means this value might be missing.
    // Under the hood, Option<String> takes up 24 bytes on the 64-bit stack
    // (a pointer to the heap, a capacity, and a length), plus whatever the text size is on the heap.
    pub token: Option<String>,
    pub api_url: Option<String>,
    pub workspace_id: Option<String>,
    pub oidc_sub: Option<String>,
    pub oidc_client_id: Option<String>,
    pub oidc_device_auth_url: Option<String>,
    pub oidc_token_url: Option<String>,
    pub oidc_userinfo_url: Option<String>,
    pub oidc_revoke_url: Option<String>,
    pub access_token: Option<String>,
}

impl Config {
    // load() takes NO arguments and returns a Result containing 'Self' (which means Config).
    // Because we are returning 'Self', this function is allocating NEW memory on the heap
    // and passing OWNERSHIP of that memory to whoever called this function.
    pub fn load() -> Result<Self> {
        // config_path() returns an owned PathBuf. We use the '?' operator to say:
        // "If this fails, instantly return the error. If it succeeds, unwrap the path."
        let path = config_path()?;

        if !path.exists() {
            // Self::default() creates a new Config where all fields are 'None'.
            // Ok(...) wraps it in the success variant of our Result enum.
            return Ok(Self::default());
        }

        // fs::read_to_string goes to the hard drive, figures out how big the file is,
        // asks the OS for that much Heap memory, reads the bytes into that memory,
        // and gives us an owned String. 'contents' now OWNS that heap memory.
        let contents = fs::read_to_string(&path).context("failed to read config file")?;

        // serde parses the 'contents' string. It allocates BRAND NEW heap memory for
        // each of the Option<String> fields in our Config struct.
        // Once this function ends, the 'contents' variable goes out of scope and its memory
        // is instantly freed (dropped), but the newly created Config is returned safely.
        serde_yaml::from_str(&contents).context("failed to parse config file")
    }

    // &self means "I want to BORROW this Config temporarily. I promise not to change it."
    // We are passing a pointer to the existing struct, not copying the data.
    pub fn save(&self) -> Result<()> {
        let path = config_path()?;

        // path.parent() returns an Option<&Path>. It's borrowing a view into the path we already own.
        // 'if let Some(parent)' is a safe way to say "If there is a parent directory, do this:"
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("failed to create config directory")?;
        }

        // We pass '&self' to serde. It reads our borrowed memory and creates a NEW
        // String on the heap containing the YAML formatted text.
        let contents = serde_yaml::to_string(self).context("failed to serialize config")?;

        // fs::write opens the file, dumps the 'contents' bytes into it, and closes it.
        // 'contents' is then dropped from memory.
        fs::write(&path, contents).context("failed to write config file")
    }

    // This returns '&str', which is a "string slice".
    // A string slice is just a pointer to some bytes in memory, plus a length.
    // We are returning a reference to data that ALREADY exists inside the Config struct.
    pub fn api_url(&self) -> &str {
        // Here is the magic sequence:
        // 1. self.api_url is an Option<String>.
        // 2. as_deref() converts &Option<String> into Option<&str>.
        //    Instead of looking at the heap-allocated String object, it gives us a direct
        //    pointer to the raw text characters on the heap.
        // 3. unwrap_or() says "If we have a pointer, use it. If not, use this hardcoded pointer."
        //    The hardcoded string "http://..." is baked directly into the final binary executable!
        self.api_url.as_deref().unwrap_or("http://localhost:7070")
    }

    // Returns a Result containing a borrowed string slice.
    pub fn require_token(&self) -> Result<&str> {
        // match forces us to handle both possibilities of the Option enum.
        match self.token.as_deref() {
            // If it exists, return the borrowed pointer wrapped in Ok()
            Some(t) => Ok(t),
            // If it's None, use the 'bail!' macro from anyhow to instantly return an Error.
            None => bail!("not logged in — run: flux login"),
        }
    }
}

// PathBuf is the file-path equivalent of a String.
// It is an owned, growable, heap-allocated buffer.
// (Conversely, &Path is the equivalent of &str — just a borrowed view).
pub fn config_path() -> Result<PathBuf> {
    // dirs::home_dir() asks the OS for the user's home path (e.g., "/home/kendrick").
    let home = dirs::home_dir().context("could not determine home directory")?;

    // .join() allocates new memory to combine the paths together into a final string.
    Ok(home.join(".config").join("flux").join("config.yaml"))
}
