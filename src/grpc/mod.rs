pub mod admin;
pub mod tls;

pub mod proto {
    tonic::include_proto!("proto.admin");
}
