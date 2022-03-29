use anyhow::{bail, Context, Ok, Result};
use indoc::formatdoc;
use std::{
    fs::{self, File},
    io::Write,
    path::PathBuf,
    process::Command,
};
use uuid::Uuid;

use crate::builders::Builder;

#[derive(Debug, Clone)]
pub struct AppSource {
    pub source: PathBuf,
    pub paths: Vec<PathBuf>,
}

impl AppSource {
    pub fn includes_file(&self, name: &str) -> bool {
        for path in &self.paths {
            if path.file_name().unwrap() == name {
                return true;
            }
        }

        false
    }
}

pub struct AppBuilder<'a> {
    name: Option<String>,
    app: AppSource,
    custom_build_cmd: Option<String>,
    custom_start_cmd: Option<String>,
    pkgs: Vec<String>,
    builder: Option<&'a dyn Builder>,
}

impl<'a> AppBuilder<'a> {
    pub fn new(
        name: Option<String>,
        source: PathBuf,
        custom_build_cmd: Option<String>,
        custom_start_cmd: Option<String>,
        pkgs: Vec<String>,
    ) -> Result<AppBuilder<'a>> {
        let dir = fs::read_dir(source.clone()).context("Failed to read app source directory")?;

        let paths: Vec<PathBuf> = dir.map(|path| path.unwrap().path()).collect();

        Ok(AppBuilder {
            name,
            app: AppSource { source, paths },
            custom_build_cmd,
            custom_start_cmd,
            pkgs,
            builder: None,
        })
    }

    pub fn detect(&mut self, builders: Vec<&'a dyn Builder>) -> Result<()> {
        println!("=== Detecting ===");

        for builder in builders {
            let matches = builder.detect(&self.app)?;
            if matches {
                self.builder = Some(builder);
                break;
            }
        }

        match self.builder {
            Some(builder) => println!("  -> Matched builder {}", builder.name()),
            None => {
                // If no builder is found, only fail if there is no start command
                if self.custom_start_cmd.is_none() {
                    bail!("Failed to match a builder")
                }

                println!("  -> No builders matched")
            }
        }

        Ok(())
    }

    pub fn build(&self) -> Result<()> {
        println!("\n=== Building ===");

        let nix_expression = self.gen_nix()?;
        println!("  -> Generated Nix expression");

        let dockerfile = self.gen_dockerfile()?;
        println!("  -> Generated Dockerfile");

        let id = Uuid::new_v4();
        let tmp_dir_name = format!("./tmp/{}", id);

        println!("  -> Copying source to tmp dir");

        let source = self.app.source.as_path().to_str().unwrap();
        let mut copy_cmd = Command::new("cp")
            .arg("-R")
            .arg(source)
            .arg(tmp_dir_name.clone())
            .spawn()?;
        copy_cmd.wait().context("Copying app source to tmp dir")?;

        println!("  -> Writing environment.nix");

        let nix_path = PathBuf::from(tmp_dir_name.clone()).join(PathBuf::from("environment.nix"));
        let mut nix_file = File::create(nix_path).context("Creating Nix environment file")?;
        nix_file
            .write_all(nix_expression.as_bytes())
            .context("Unable to write Nix expression")?;

        println!("  -> Writing Dockerfile");

        let dockerfile_path = PathBuf::from(tmp_dir_name.clone()).join(PathBuf::from("Dockerfile"));
        File::create(dockerfile_path.clone()).context("Creating Dockerfile file")?;
        fs::write(dockerfile_path, dockerfile).context("Writing Dockerfile")?;

        // println!(
        //     "\nRun:\n  docker build {} -t {}",
        //     tmp_dir_name.as_str(),
        //     self.name.clone().unwrap_or(id.to_string())
        // );

        println!("  -> Building image");

        let name = self.name.clone().unwrap_or_else(|| id.to_string());

        let mut docker_build_cmd = Command::new("docker")
            .arg("build")
            .arg(tmp_dir_name.as_str())
            .arg("-t")
            .arg(name.clone())
            .spawn()?;

        docker_build_cmd.wait().context("Building image")?;

        println!("  -> Built!");

        println!("\nRun:\n  docker run {}", name);

        Ok(())
    }

    pub fn gen_nix(&self) -> Result<String> {
        // let build_inputs = &self
        //     .participating_builders
        //     .iter()
        //     .map(|builder| {
        //         let inputs = builder.build_inputs();
        //         inputs
        //     })
        //     .flatten()
        //     .collect::<Vec<String>>();

        let user_pkgs = self
            .pkgs
            .iter()
            .map(|s| format!("pkgs.{}", s))
            .collect::<Vec<String>>()
            .join(" ");

        let pkgs = match self.builder {
            Some(builder) => format!("{} {}", builder.build_inputs(&self.app), user_pkgs),
            None => user_pkgs,
        };

        // let nix_expression = formatdoc! {"
        //   {{ pkgs ? import <nixpkgs> {{ }} }}:

        //   pkgs.mkShell {{
        //     buildInputs = [ {pkgs} ];
        //   }}
        // ",
        // pkgs=pkgs};

        let nix_expression = formatdoc! {"
          with import <nixpkgs> {{ }}; [ {pkgs} ]
        ",
        pkgs=pkgs};

        Ok(nix_expression)
    }

    pub fn gen_dockerfile(&self) -> Result<String> {
        // let builder = self.builder.expect("Cannot build without builder");

        let install_cmd = match self.builder {
            Some(builder) => builder
                .install_cmd(&self.app)?
                .unwrap_or_else(|| "".to_string()),
            None => "".to_string(),
        };

        let suggested_build_cmd = match self.builder {
            Some(builder) => builder
                .suggested_build_cmd(&self.app)?
                .unwrap_or_else(|| "".to_string()),
            None => "".to_string(),
        };
        let build_cmd = self.custom_build_cmd.clone().unwrap_or(suggested_build_cmd);

        let suggested_start_cmd = match self.builder {
            Some(builder) => builder
                .suggested_start_command(&self.app)?
                .unwrap_or_else(|| "".to_string()),
            None => "".to_string(),
        };
        let start_cmd = self.custom_start_cmd.clone().unwrap_or(suggested_start_cmd);

        let dockerfile = formatdoc! {"
          FROM nixos/nix

          RUN nix-channel --update

          COPY . /app
          WORKDIR /app

          # Load Nix environment
          RUN nix-env -if environment.nix

          # Install
          RUN {install_cmd}

          # Build
          RUN {build_cmd}

          # Start
          CMD {start_cmd}
        ",
        install_cmd=install_cmd,
        build_cmd=build_cmd,
        start_cmd=start_cmd};

        Ok(dockerfile)
    }
}