Undo a completed symbolic edit in the bound local checkout using the `edit_id`
returned by that action. Retained private preimages are restored only while
every current file matches its recorded postimage hash and the checkout lease
still belongs to the same opaque source identity. This mutating action uses the
host's normal approval path and is safe to retry.
