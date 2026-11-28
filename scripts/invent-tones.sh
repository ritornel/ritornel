#!/usr/bin/env bash
# Makes an invented library that plays and syncs: albums of tones with
# covers, by invented artists, as real audio files. `ritornel-server invent`
# writes no audio, so nothing it makes can be played, encoded or copied.
#
#   scripts/invent-tones.sh <folder> [<albums>]
#
# Each track is a tone with a little noise under it, so that a FLAC of it
# has a size worth copying, 20 to 40 seconds long. Most albums are FLAC with
# ReplayGain tags and a folder cover; every fifth is MP3, for a lossy master
# that is sent as it is; one has two discs. The same folder and count make
# the same library. It needs the ffmpeg that mise pins.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:?usage: $0 <folder> [<albums>]}
albums=${2:-18}

artists=("Ada Vale" "The Paper Lanterns" "Corin Marsh" "Small Hours Club" "Odile Brandt"
    "Tessellate" "Juniper & the Weather" "Halden Row" "Mira Solano")
firsts=("Low" "Night" "Glass" "Salt" "Paper" "Slow" "Winter" "Lamp" "Tide" "Quiet" "Far" "Open")
seconds=("Tide" "Ferry" "Orchard" "Hours" "Rooms" "Weather" "Branches" "Oil" "Water" "Signals"
    "Fields" "Letters")
titles=("Shoreline" "Lamp Oil" "Weathervane" "Tidewater" "Salt Hours" "Glass Orchard" "Night Ferry"
    "Drifting" "Juniper Hill" "Slow Rooms" "Paper Boats" "Open Water" "Far Signals" "Quiet Fields"
    "Winter Branches" "Low Sun" "Harbour Lights" "Small Hours" "Long Way Round" "Undertow")

ffmpeg() {
    (cd "$root" && mise exec -- ffmpeg -loglevel error -y "$@")
}

mkdir -p "$out"
out=$(cd "$out" && pwd)
for ((a = 0; a < albums; a++)); do
    artist=${artists[$((a % ${#artists[@]}))]}
    album="${firsts[$(((a * 5 + 1) % ${#firsts[@]}))]} ${seconds[$(((a * 7 + 2) % ${#seconds[@]}))]}"
    year=$((2004 + (a * 3) % 21))
    tracks=$((6 + a % 4))
    folder="$out/$artist/$year - $album"
    mkdir -p "$folder"
    # A cover of two colours, its own for each album.
    one=$(printf '%02x%02x%02x' $(((a * 53 + 40) % 200 + 30)) $(((a * 97 + 80) % 180 + 40)) $(((a * 31 + 120) % 160 + 60)))
    two=$(printf '%02x%02x%02x' $(((a * 71 + 150) % 190 + 40)) $(((a * 29 + 30) % 170 + 30)) $(((a * 113 + 60) % 200 + 30)))
    ffmpeg -f lavfi -i "gradients=s=1000x1000:c0=0x$one:c1=0x$two:x0=$(((a * 211) % 1000)):y0=0:x1=$(((a * 367) % 1000)):y1=1000:d=1" \
        -frames:v 1 -q:v 3 "$folder/cover.jpg"
    for ((t = 1; t <= tracks; t++)); do
        title=${titles[$(((a * 3 + t * 7) % ${#titles[@]}))]}
        seconds_long=$((20 + (a * 7 + t * 5) % 21))
        hz=$((110 + ((a * 13 + t * 29) % 24) * 20))
        disc=1
        # The seventh album has two discs.
        if ((a == 6 && t > tracks / 2)); then disc=2; fi
        number=$(printf '%02d' "$t")
        tags=(-metadata "artist=$artist" -metadata "album_artist=$artist" -metadata "album=$album"
            -metadata "title=$title" -metadata "track=$t" -metadata "disc=$disc" -metadata "date=$year")
        sound=(-f lavfi -i "sine=frequency=$hz:duration=$seconds_long"
            -f lavfi -i "anoisesrc=color=pink:amplitude=0.03:duration=$seconds_long:seed=$((a * 100 + t))"
            -filter_complex "[0][1]amix=inputs=2:normalize=0,aformat=channel_layouts=stereo")
        if ((a % 5 == 4)); then
            ffmpeg "${sound[@]}" "${tags[@]}" -codec:a libmp3lame -b:a 256k "$folder/$number $title.mp3"
        else
            ffmpeg "${sound[@]}" "${tags[@]}" \
                -metadata "REPLAYGAIN_TRACK_GAIN=-$((3 + t % 4)).$((a % 10))0 dB" \
                -metadata "REPLAYGAIN_TRACK_PEAK=0.8$((a % 10))$((t % 10))000" \
                -metadata "REPLAYGAIN_ALBUM_GAIN=-4.$((a % 10))0 dB" \
                -metadata "REPLAYGAIN_ALBUM_PEAK=0.9$((a % 10))0000" \
                -sample_fmt s16 -ar 44100 "$folder/$number $title.flac"
        fi
    done
done
echo "made $albums albums in $out"
