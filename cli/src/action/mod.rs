// Copyright 2018-2021 Cargill Incorporated
// Copyright 2018 Intel Corporation
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

pub mod admin;
mod api;
pub mod certs;
pub mod circuit;
#[cfg(feature = "database")]
pub mod database;
#[cfg(feature = "health")]
pub mod health;
pub mod keygen;
#[cfg(feature = "authorization-handler-maintenance")]
pub mod maintenance;
#[cfg(feature = "permissions")]
pub mod permissions;
#[cfg(feature = "authorization-handler-rbac")]
pub mod rbac;
pub mod registry;

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::File;
use std::io::{Error as IoError, ErrorKind, Read};
use std::path::Path;

use clap::ArgMatches;
use cylinder::jwt::JsonWebTokenBuilder;

use super::error::CliError;
use crate::signing::load_signer;

const DEFAULT_SPLINTER_REST_API_URL: &str = "http://127.0.0.1:8080";
const SPLINTER_REST_API_URL_ENV: &str = "SPLINTER_REST_API_URL";

/// A CLI Command Action.
///
/// An Action is a single subcommand for CLI operations.
pub trait Action {
    /// Run a CLI Action with the given args
    fn run<'a>(&mut self, arg_matches: Option<&ArgMatches<'a>>) -> Result<(), CliError>;
}

/// A collection of Subcommands associated with a single parent command.
#[derive(Default)]
pub struct SubcommandActions<'a> {
    actions: HashMap<String, Box<dyn Action + 'a>>,
}

impl<'a> SubcommandActions<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_command<'action: 'a, A: Action + 'action>(
        mut self,
        command: &str,
        action: A,
    ) -> Self {
        self.actions.insert(command.to_string(), Box::new(action));

        self
    }
}

impl<'s> Action for SubcommandActions<'s> {
    fn run<'a>(&mut self, arg_matches: Option<&ArgMatches<'a>>) -> Result<(), CliError> {
        let args = arg_matches.ok_or(CliError::RequiresArgs)?;

        let (subcommand, args) = args.subcommand();

        if let Some(action) = self.actions.get_mut(subcommand) {
            action.run(args)
        } else {
            Err(CliError::InvalidSubcommand)
        }
    }
}

fn chown(path: &Path, uid: u32, gid: u32) -> Result<(), CliError> {
    let pathstr = path
        .to_str()
        .ok_or_else(|| CliError::EnvironmentError(format!("Invalid path: {:?}", path)))?;
    let cpath =
        CString::new(pathstr).map_err(|err| CliError::EnvironmentError(format!("{}", err)))?;
    let result = unsafe { libc::chown(cpath.as_ptr(), uid, gid) };
    match result {
        0 => Ok(()),
        code => Err(CliError::EnvironmentError(format!(
            "Error chowning file {}: {}",
            pathstr, code
        ))),
    }
}

/// Reads a private key from the given file name.
fn read_private_key(file_name: &str) -> Result<String, CliError> {
    let mut file = File::open(file_name).map_err(|err| {
        CliError::EnvironmentError(format!(
            "Unable to open key file '{}': {}",
            file_name,
            msg_from_io_error(err)
        ))
    })?;

    let mut buf = String::new();
    file.read_to_string(&mut buf).map_err(|err| {
        CliError::EnvironmentError(format!(
            "Unable to read key file '{}': {}",
            file_name,
            msg_from_io_error(err)
        ))
    })?;
    let key = buf.trim().to_string();

    Ok(key)
}

fn msg_from_io_error(err: IoError) -> String {
    match err.kind() {
        ErrorKind::NotFound => "File not found".into(),
        ErrorKind::PermissionDenied => "Permission denied".into(),
        ErrorKind::InvalidData => "Invalid data".into(),
        _ => "Unknown I/O error".into(),
    }
}

// build a signed json web token using the private key
fn create_cylinder_jwt_auth(key_name: Option<&str>) -> Result<String, CliError> {
    let signer = load_signer(key_name)?;

    let encoded_token = JsonWebTokenBuilder::new()
        .build(&*signer)
        .map_err(|err| CliError::ActionError(format!("failed to build json web token: {}", err)))?;

    Ok(format!("Bearer Cylinder:{}", encoded_token))
}

// Takes a vec of vecs of strings. The first vec should include the title of the columns.
// The max length of each column is calculated and is used as the column with when printing the
// table.
fn print_table(table: Vec<Vec<String>>) {
    let mut max_lengths = Vec::new();

    // find the max lengths of the columns
    for row in table.iter() {
        for (i, col) in row.iter().enumerate() {
            if let Some(length) = max_lengths.get_mut(i) {
                if col.len() > *length {
                    *length = col.len()
                }
            } else {
                max_lengths.push(col.len())
            }
        }
    }

    // print each row with correct column size
    for row in table.iter() {
        let mut col_string = String::from("");
        for (i, len) in max_lengths.iter().enumerate() {
            if let Some(value) = row.get(i) {
                col_string += &format!("{}{} ", value, " ".repeat(*len - value.len()),);
            } else {
                col_string += &" ".repeat(*len);
            }
        }
        println!("{}", col_string);
    }
}
