// Copyright 2021 Datafuse Labs.
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

//! Meta service impl a grpc server that serves both raft protocol: append_entries, vote and install_snapshot.
//! It also serves RPC for user-data access.

use std::convert::TryInto;
use std::pin::Pin;
use std::sync::Arc;

use common_arrow::arrow_format::flight::data::BasicAuth;
use common_flight_rpc::FlightClaim;
use common_flight_rpc::FlightToken;
use common_meta_raft_store::message::ForwardRequest;
use common_meta_raft_store::protobuf::meta_service_server::MetaService;
use common_meta_raft_store::protobuf::GetReply;
use common_meta_raft_store::protobuf::GetReq;
use common_meta_raft_store::protobuf::HandshakeRequest;
use common_meta_raft_store::protobuf::HandshakeResponse;
use common_meta_raft_store::protobuf::RaftReply;
use common_meta_raft_store::protobuf::RaftRequest;
use common_meta_raft_store::state_machine::AppliedState;
use common_meta_types::LogEntry;
use common_tracing::tracing;
use futures::StreamExt;
use prost::Message;
use tonic::codegen::futures_core::Stream;
use tonic::metadata::MetadataMap;
use tonic::Request;
use tonic::Response;
use tonic::Status;
use tonic::Streaming;

use crate::meta_service::ForwardRequestBody;
use crate::meta_service::MetaNode;

pub type GrpcStream<T> =
    Pin<Box<dyn Stream<Item = Result<T, tonic::Status>> + Send + Sync + 'static>>;

pub struct MetaServiceImpl {
    token: FlightToken,
    pub meta_node: Arc<MetaNode>,
}

impl MetaServiceImpl {
    pub fn create(meta_node: Arc<MetaNode>) -> Self {
        Self {
            token: FlightToken::create(),
            meta_node,
        }
    }

    fn check_token(&self, metadata: &MetadataMap) -> Result<FlightClaim, Status> {
        let token = metadata
            .get_bin("auth-token-bin")
            .and_then(|v| v.to_bytes().ok())
            .and_then(|b| String::from_utf8(b.to_vec()).ok())
            .ok_or_else(|| Status::internal("Error auth-token-bin is empty"))?;

        let claim = self
            .token
            .try_verify_token(token)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(claim)
    }
}

#[async_trait::async_trait]
impl MetaService for MetaServiceImpl {
    // rpc handshake related type
    type HandshakeStream = GrpcStream<HandshakeResponse>;

    // rpc handshake first
    #[tracing::instrument(level = "info", skip(self))]
    async fn handshake(
        &self,
        request: Request<Streaming<HandshakeRequest>>,
    ) -> Result<Response<Self::HandshakeStream>, Status> {
        let req = request
            .into_inner()
            .next()
            .await
            .ok_or_else(|| Status::internal("Error request next is None"))??;

        let HandshakeRequest { payload, .. } = req;
        let auth = BasicAuth::decode(&*payload).map_err(|e| Status::internal(e.to_string()))?;

        let user = "root";
        if auth.username == user {
            let claim = FlightClaim {
                username: user.to_string(),
            };
            let token = self
                .token
                .try_create_token(claim)
                .map_err(|e| Status::internal(e.to_string()))?;

            let resp = HandshakeResponse {
                payload: token.into_bytes(),
                ..HandshakeResponse::default()
            };
            let output = futures::stream::once(async { Ok(resp) });
            Ok(Response::new(Box::pin(output)))
        } else {
            Err(Status::unauthenticated(format!(
                "Unknown user: {}",
                auth.username
            )))
        }
    }

    /// Handles a write request.
    /// This node must be leader or an error returned.
    #[tracing::instrument(level = "info", skip(self))]
    async fn write(
        &self,
        request: tonic::Request<RaftRequest>,
    ) -> Result<tonic::Response<RaftReply>, tonic::Status> {
        // self.check_token(request.metadata())?;
        common_tracing::extract_remote_span_as_parent(&request);

        let mes = request.into_inner();
        let ent: LogEntry = mes.try_into()?;

        // TODO(xp): call meta_node.write()
        let res = self
            .meta_node
            .handle_forwardable_request(ForwardRequest {
                forward_to_leader: 1,
                body: ForwardRequestBody::Write(ent),
            })
            .await;

        let res = res.map(|x| {
            let a: AppliedState = x.try_into().unwrap();
            a
        });

        let raft_reply = RaftReply::from(res);
        return Ok(tonic::Response::new(raft_reply));
    }

    #[tracing::instrument(level = "info", skip(self))]
    async fn get(
        &self,
        request: tonic::Request<GetReq>,
    ) -> Result<tonic::Response<GetReply>, tonic::Status> {
        // TODO(xp): this method should be removed along with DFS
        // self.check_token(request.metadata())?;
        common_tracing::extract_remote_span_as_parent(&request);

        let req = request.into_inner();
        let rst = GetReply {
            ok: false,
            key: req.key,
            value: "".into(),
        };

        Ok(tonic::Response::new(rst))
    }

    #[tracing::instrument(level = "info", skip(self))]
    async fn forward(
        &self,
        request: tonic::Request<RaftRequest>,
    ) -> Result<tonic::Response<RaftReply>, tonic::Status> {
        self.check_token(request.metadata())?;
        common_tracing::extract_remote_span_as_parent(&request);

        let req = request.into_inner();

        let admin_req: ForwardRequest = serde_json::from_str(&req.data)
            .map_err(|x| tonic::Status::invalid_argument(x.to_string()))?;

        let res = self.meta_node.handle_forwardable_request(admin_req).await;

        let raft_mes: RaftReply = res.into();

        Ok(tonic::Response::new(raft_mes))
    }

    #[tracing::instrument(level = "info", skip(self, request))]
    async fn append_entries(
        &self,
        request: tonic::Request<RaftRequest>,
    ) -> Result<tonic::Response<RaftReply>, tonic::Status> {
        common_tracing::extract_remote_span_as_parent(&request);

        let req = request.into_inner();

        let ae_req =
            serde_json::from_str(&req.data).map_err(|x| tonic::Status::internal(x.to_string()))?;

        let resp = self
            .meta_node
            .raft
            .append_entries(ae_req)
            .await
            .map_err(|x| tonic::Status::internal(x.to_string()))?;
        let data = serde_json::to_string(&resp).expect("fail to serialize resp");
        let mes = RaftReply {
            data,
            error: "".to_string(),
        };

        Ok(tonic::Response::new(mes))
    }

    #[tracing::instrument(level = "info", skip(self, request))]
    async fn install_snapshot(
        &self,
        request: tonic::Request<RaftRequest>,
    ) -> Result<tonic::Response<RaftReply>, tonic::Status> {
        common_tracing::extract_remote_span_as_parent(&request);

        let req = request.into_inner();

        let is_req =
            serde_json::from_str(&req.data).map_err(|x| tonic::Status::internal(x.to_string()))?;

        let resp = self
            .meta_node
            .raft
            .install_snapshot(is_req)
            .await
            .map_err(|x| tonic::Status::internal(x.to_string()))?;
        let data = serde_json::to_string(&resp).expect("fail to serialize resp");
        let mes = RaftReply {
            data,
            error: "".to_string(),
        };

        Ok(tonic::Response::new(mes))
    }

    #[tracing::instrument(level = "info", skip(self, request))]
    async fn vote(
        &self,
        request: tonic::Request<RaftRequest>,
    ) -> Result<tonic::Response<RaftReply>, tonic::Status> {
        common_tracing::extract_remote_span_as_parent(&request);

        let req = request.into_inner();

        let v_req =
            serde_json::from_str(&req.data).map_err(|x| tonic::Status::internal(x.to_string()))?;

        let resp = self
            .meta_node
            .raft
            .vote(v_req)
            .await
            .map_err(|x| tonic::Status::internal(x.to_string()))?;
        let data = serde_json::to_string(&resp).expect("fail to serialize resp");
        let mes = RaftReply {
            data,
            error: "".to_string(),
        };

        Ok(tonic::Response::new(mes))
    }
}
