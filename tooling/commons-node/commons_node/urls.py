"""URL routing for the commons protocol (spec/commons.md, /v0/ binding)."""

from django.urls import path, re_path

from commons import views

# Content-address: <prefix>_<64 lowercase hex>.
_ADDR = r"(?P<address>[a-z]+_[0-9a-f]{64})"

urlpatterns = [
    path("v0/records", views.records),                 # POST publish
    re_path(rf"^v0/records/{_ADDR}$", views.record),    # GET resolve / HEAD exists
    re_path(rf"^v0/records/{_ADDR}/certifications$", views.certifications),  # GET certs about a function
    re_path(rf"^v0/records/{_ADDR}/attestations$", views.attestations),  # GET eval attestations about weights
    re_path(rf"^v0/records/{_ADDR}/equivalences$", views.equivalences),  # GET equivalence claims about a function
    re_path(r"^v0/blobs/(?P<sha256>[0-9a-f]{64})$", views.blob),  # GET binary blob by content hash
    path("v0/query", views.query),                      # POST typed discovery
    path("v0/search", views.search),                    # POST semantic discovery
    path("v0/prove", views.prove),                      # POST prove a record's properties (best-effort)
    path("v0/equiv", views.equiv),                      # POST prove two functions equivalent (best-effort)
    path("v0/sync", views.sync),                        # GET replication feed
    path("v0/sync/merkle", views.sync_merkle),          # GET Merkle set reconciliation
    path("v0/anchors", views.anchors),                  # GET signed Merkle-root anchors
    path("v0/info", views.info),                        # GET node metadata
]
