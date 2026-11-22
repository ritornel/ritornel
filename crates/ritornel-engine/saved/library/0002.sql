INSERT INTO library_revision (rev, command_id, method, client_session_pid, client_kind, committed_at_ms)
VALUES
  (1, NULL, 'library.create', NULL, NULL, 1794776400000),
  (2, NULL, 'scan.resolve', NULL, NULL, 1794776410000),
  (3, NULL, 'history.ingest', NULL, NULL, 1794776420000);

INSERT INTO server (id, public_id, name, created_at_ms, rev)
VALUES (1, X'01010101010101010101010101010101', 'invented-hub', 1794776400000, 1);

INSERT INTO library_meta (id, library_id, hub_server_id, hub_epoch, rev, created_at_ms)
VALUES (1, X'02020202020202020202020202020202', 1, 1, 3, 1794776400000);

INSERT INTO library_setting (id, key, value_json, rev)
VALUES (1, 'identity.versions', '{"extractor":1,"resolver":2,"normalizer":1}', 2);

INSERT INTO metadata_source (id, kind, precedence, rev)
VALUES
  (1, 'user', 1, 1),
  (2, 'musicbrainz', 2, 1),
  (3, 'beets', 3, 1),
  (4, 'file_tags', 4, 1),
  (5, 'device', 5, 1),
  (6, 'inferred', 6, 1);

INSERT INTO library_root (id, public_id, server_id, name, kind, manage_tags, manage_layout, created_at_ms, rev)
VALUES (1, X'03030303030303030303030303030303', 1, 'main', 'master', 0, 0, 1794776400000, 1);

INSERT INTO source_instance (id, public_id, kind, name, root_id, created_at_ms, rev)
VALUES
  (1, X'04040404040404040404040404040404', 'folder', 'main', 1, 1794776400000, 1),
  (2, X'05050505050505050505050505050505', 'lastfm', 'invented-user', NULL, 1794776400000, 1);

INSERT INTO artist (id, public_id, redirect_to_id, canonical_id, display_name, display_sort_name, match_name, rev)
VALUES
  (1, X'11111111111111111111111111111111', NULL, 1, 'Moss Arcade', 'Moss Arcade', 'moss arcade', 2),
  (2, X'22222222222222222222222222222222', NULL, 2, 'Glass Harbor', 'Glass Harbor', 'glass harbor', 2),
  (3, X'33333333333333333333333333333333', 1, 1, 'Moss', 'Moss', 'moss', 2);

INSERT INTO artist_credit (id, display_text, rev)
VALUES
  (1, 'Moss Arcade', 2),
  (2, 'Glass Harbor', 2);

INSERT INTO artist_credit_name (id, artist_credit_id, position, artist_id, credited_name, join_phrase, rev)
VALUES
  (1, 1, 0, 1, 'Moss Arcade', '', 2),
  (2, 2, 0, 2, 'Glass Harbor', '', 2);

INSERT INTO recording (id, public_id, redirect_to_id, canonical_id, display_title, display_artist_credit_id, rev)
VALUES
  (1, X'41414141414141414141414141414141', NULL, 1, 'Paper Kites', 1, 2),
  (2, X'42424242424242424242424242424242', NULL, 2, 'Salt Lantern', 2, 2);

INSERT INTO release_group (id, public_id, presentation, preferred_release_id, display_release_id, display_primary_type, display_secondary_types_json, display_first_date, display_first_year, rev)
VALUES
  (1, X'51515151515151515151515151515151', NULL, NULL, NULL, 'album', '[]', '2020', 2020, 2),
  (2, X'52525252525252525252525252525252', NULL, NULL, NULL, 'ep', '[]', '2021', 2021, 2);

INSERT INTO release (id, public_id, chosen_release_group_id, release_group_id, redirect_to_id, canonical_id, display_title, display_artist_credit_id, display_date, display_year, display_picture_id, rev)
VALUES
  (1, X'61616161616161616161616161616161', NULL, 1, NULL, 1, 'Tin Weather', 1, '2020', 2020, NULL, 2),
  (2, X'62626262626262626262626262626262', NULL, 2, NULL, 2, 'Harbor Fog', 2, '2021', 2021, NULL, 2);

UPDATE release_group SET display_release_id = 1 WHERE id = 1;
UPDATE release_group SET display_release_id = 2 WHERE id = 2;

INSERT INTO track_appearance (id, public_id, recording_id, release_id, redirect_to_id, canonical_id, display_title, display_artist_credit_id, display_disc_number, display_track_number, display_duration_ms, display_added_at_ms, rev)
VALUES
  (1, X'71717171717171717171717171717171', 1, 1, NULL, 1, 'Paper Kites', 1, 1, 1, 200000, 1794776400000, 2),
  (2, X'72727272727272727272727272727272', 2, 2, NULL, 2, 'Salt Lantern', 2, 1, 1, 210000, 1794776400000, 2);

INSERT INTO audio_asset (id, public_id, hash_scheme, audio_hash, flac_md5, codec, sample_rate_hz, channel_layout, bit_depth, bitrate_kbps, lossless, duration_ms, rev)
VALUES
  (1, X'81818181818181818181818181818181', 1, X'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', NULL, 'flac', 44100, 'stereo', 16, 900, 1, 200000, 2),
  (2, X'82828282828282828282828282828282', 1, X'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', NULL, 'flac', 44100, 'stereo', 16, 900, 1, 210000, 2);

INSERT INTO source_item (id, public_id, source_instance_id, item_kind, provider_key, current_revision_id, current_recording_id, size_bytes, mtime_ms, missing_since_ms, retired_at_ms, first_seen_at_ms, rev)
VALUES
  (1, X'91919191919191919191919191919191', 1, 'file', 'Moss Arcade/Tin Weather/01 Paper Kites.flac', NULL, 1, 1000, 1794776400000, NULL, NULL, 1794776400000, 2),
  (2, X'92929292929292929292929292929292', 1, 'file', 'Glass Harbor/Harbor Fog/01 Salt Lantern.flac', NULL, 2, 1000, 1794776400000, NULL, NULL, 1794776400000, 2),
  (3, X'93939393939393939393939393939393', 2, 'scrobble_subject', '["Moss Arcade","Tin Weather","Paper Kites"]', NULL, 1, NULL, NULL, NULL, NULL, 1794776410000, 3);

INSERT INTO source_item_revision (id, source_item_id, revision_no, payload_hash, payload_json, audio_asset_id, observed_at_ms, rev)
VALUES
  (1, 1, 1, X'10101010101010101010101010101010', '{}', 1, 1794776400000, 2),
  (2, 2, 1, X'20202020202020202020202020202020', '{}', 2, 1794776400000, 2);

UPDATE source_item SET current_revision_id = 1 WHERE id = 1;
UPDATE source_item SET current_revision_id = 2 WHERE id = 2;

INSERT INTO identifier_claim (id, source_item_id, revision_id, extractor_version, scope, namespace, value, ordinal, metadata_source_id, rev)
VALUES
  (1, 1, 1, 1, 'artist', 'mb_artist', '12121212-3434-4565-8787-909090909090', 0, 4, 2),
  (2, 1, 1, 1, 'artist', 'mb_album_artist', '12121212-3434-4565-8787-909090909090', 0, 4, 2);

INSERT INTO metadata_claim (id, source_item_id, revision_id, extractor_version, scope, field, value, ordinal, metadata_source_id, rev)
VALUES
  (1, 1, 1, 1, 'track_appearance', 'title', 'Paper Kites', 0, 4, 2),
  (2, 1, 1, 1, 'track_appearance', 'artist', 'Moss Arcade', 0, 4, 2),
  (3, 1, 1, 1, 'release', 'album', 'Tin Weather', 0, 4, 2),
  (4, 1, 1, 1, 'release', 'album_artist', 'Moss Arcade', 0, 4, 2),
  (5, 1, 1, 1, 'track_appearance', 'disc_number', '1', 0, 4, 2),
  (6, 1, 1, 1, 'track_appearance', 'track_number', '1', 0, 4, 2),
  (7, 1, 1, 1, 'track_appearance', 'duration_ms', '200000', 0, 4, 2),
  (8, 2, 2, 1, 'track_appearance', 'title', 'Salt Lantern', 0, 4, 2),
  (9, 2, 2, 1, 'track_appearance', 'artist', 'Glass Harbor', 0, 4, 2),
  (10, 2, 2, 1, 'release', 'album', 'Harbor Fog', 0, 4, 2),
  (11, 2, 2, 1, 'release', 'album_artist', 'Glass Harbor', 0, 4, 2),
  (12, 2, 2, 1, 'track_appearance', 'disc_number', '1', 0, 4, 2),
  (13, 2, 2, 1, 'track_appearance', 'track_number', '1', 0, 4, 2),
  (14, 2, 2, 1, 'track_appearance', 'duration_ms', '210000', 0, 4, 2);

INSERT INTO identity_decision (id, action, source_item_id, entity_kind, entity_id, target_entity_id, method, resolver_version, normalizer_version, score, runner_up_gap, penalty_vector_json, evidence_json, needs_review, actor_kind, actor_session_pid, rationale, decided_at_ms, rev)
VALUES
  (1, 'bind', 1, NULL, NULL, NULL, 'new', 2, 1, 0.0, NULL, NULL, '{"claims":[]}', 0, 'resolver', NULL, NULL, 1794776410000, 2),
  (2, 'bind', 2, NULL, NULL, NULL, 'new', 2, 1, 0.0, NULL, NULL, '{"claims":[]}', 0, 'resolver', NULL, NULL, 1794776410000, 2),
  (3, 'bind', 3, NULL, NULL, NULL, 'scrobble_text', 1, 1, 0.0, NULL, NULL, '{"matcher":1}', 0, 'resolver', NULL, NULL, 1794776420000, 3);

INSERT INTO assignment (id, source_item_id, recording_id, appearance_id, decision_id, effective_from_ms, superseded_at_ms, superseded_by_id, rev)
VALUES
  (1, 1, 1, 1, 1, 1794776410000, NULL, NULL, 2),
  (2, 2, 2, 2, 2, 1794776410000, NULL, NULL, 2),
  (3, 3, 1, 1, 3, 1794776420000, NULL, NULL, 3);

INSERT INTO alias_observation (id, scope, name_a, name_b, normalizer_version, decision_id, observed_at_ms, rev)
VALUES (1, 'artist', 'moss', 'moss arcade', 1, 1, 1794776410000, 2);

INSERT INTO search_text (rowid, title, artist, album)
VALUES
  (1, 'Paper Kites', 'Moss Arcade', 'Tin Weather'),
  (2, 'Salt Lantern', 'Glass Harbor', 'Harbor Fog');

INSERT INTO listener (id, public_id, name, rev)
VALUES (1, X'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1', 'Invented Listener', 1);

INSERT INTO history_endpoint (id, public_id, kind, server_id, name, rev)
VALUES
  (1, X'b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1', 'player', 1, 'invented-hub', 1),
  (2, X'b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2', 'manual', NULL, 'manual', 1),
  (3, X'b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3', 'service', NULL, 'lastfm:invented-user', 1);

INSERT INTO service_account (id, public_id, service, account_name, listener_id, endpoint_id, source_instance_id, created_at_ms, rev)
VALUES (1, X'c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1', 'lastfm', 'invented-user', 1, 3, 2, 1794776400000, 1);

INSERT INTO listener_zone_period (id, listener_id, zone_name, valid_from_ms, valid_to_ms, rev)
VALUES (1, 1, 'America/New_York', NULL, NULL, 1);

INSERT INTO history_observation (id, endpoint_id, source_key, kind, source_item_id, listener_id, captured_at_utc_ms, received_at_utc_ms, sent_at_utc_ms, sent_mono_ms, sent_boot_id, clock_skew_ms, zone_name, zone_basis, utc_offset_minutes, local_date, local_hour, payload_json, rev)
VALUES
  (1, 3, X'd1d1d1d1d1d1d1d1', 'import', 3, 1, 1794776420000, 1794776420000, NULL, NULL, NULL, NULL, NULL, 'none', NULL, NULL, NULL, '{}', 3),
  (2, 3, X'd2d2d2d2d2d2d2d2', 'import', 3, 1, 1794776420000, 1794776420000, NULL, NULL, NULL, NULL, NULL, 'none', NULL, NULL, NULL, '{}', 3),
  (3, 2, X'e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1', 'correction', NULL, 1, 1794776420000, 1794776420000, NULL, NULL, NULL, NULL, NULL, 'none', NULL, NULL, NULL, '{}', 3);

INSERT INTO observation_import (observation_id, played_at_utc_ms, artist, album, title, artist_mbid, album_mbid, track_mbid, rev)
VALUES
  (1, 1794776400000, 'Moss Arcade', 'Tin Weather', 'Paper Kites', NULL, NULL, NULL, 3),
  (2, 1794777000000, 'Moss Arcade', 'Tin Weather', 'Paper Kites', NULL, NULL, NULL, 3);

INSERT INTO observation_correction (observation_id, verb, set_listener_id, target_endpoint_id, target_from_utc_ms, target_to_utc_ms, clock_offset_ms, note, actor_session_pid, rev)
VALUES (3, 'exclude', NULL, NULL, NULL, NULL, NULL, 'skip duplicate', NULL, 3);

INSERT INTO correction_target (id, correction_id, target_observation_id, rev)
VALUES (1, 3, 2, 3);

INSERT INTO listener_rating (id, listener_id, recording_id, source_item_id, kind, value, cleared, endpoint_id, actor_kind, actor_session_pid, rated_at_ms, rev)
VALUES (1, 1, 1, NULL, 'rating', 80, 0, NULL, 'person', NULL, 1794776420000, 3);
