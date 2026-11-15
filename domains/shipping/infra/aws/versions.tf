terraform {
  required_version = ">= 1.10.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.73"
    }
  }

  # Backend parcial: terraform init -backend-config=envs/<ambiente>.s3.tfbackend
  backend "s3" {}
}

provider "aws" {
  region                      = var.region
  skip_credentials_validation = var.environment == "local"
  s3_use_path_style           = var.environment == "local"
  default_tags {
    tags = {
      "saci:project"     = "saci"
      "saci:repository"  = "saci-commerce"
      "saci:service"     = "shipping"
      "saci:environment" = var.environment
      "saci:managed-by"  = "terraform"
    }
  }
}
