variable "environment" {
  description = "local, dev, staging ou prod."
  type        = string
}

variable "region" {
  description = "Região."
  type        = string
  default     = "sa-east-1"
}
