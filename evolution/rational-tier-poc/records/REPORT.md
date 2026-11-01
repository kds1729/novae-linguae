| record | run | certified | typecheck | effects | refinement | termination | complexity | properties (generative) | prove |
|---|---|---|---|---|---|---|---|---|---|
| `rat_normalize` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND / post[2]=SOUND | SOUND | SOUND | value_preserved=EXHAUSTIVE; idempotent=EXHAUSTIVE; cross=HELD; cross_abs=HELD; unique=HELD | value_preserved=PROVED; idempotent=PROVED; cross=PROVED; cross_abs=PROVED; unique=PROVED |
| `rat_coprime_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | lowest_terms=EXHAUSTIVE | lowest_terms=PROVED |
| `rat_neg` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND | SOUND | SOUND | involution_num=EXHAUSTIVE; involution_den=EXHAUSTIVE | involution_num=PROVED; involution_den=PROVED |
| `rat_inv` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND | SOUND | SOUND | denominator_positive=EXHAUSTIVE | denominator_positive=PROVED |
| `rat_add` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND | SOUND | SOUND | commutative=HELD; congruent=HELD | commutative=PROVED; congruent=PROVED |
| `rat_sub` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND | SOUND | SOUND | self_difference_is_zero=EXHAUSTIVE | self_difference_is_zero=PROVED |
| `rat_mul` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND | SOUND | SOUND | commutative=HELD; zero_annihilates=EXHAUSTIVE | commutative=PROVED; zero_annihilates=PROVED |
| `rat_eq` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND | SOUND | SOUND | reflexive=EXHAUSTIVE; symmetric=HELD | reflexive=PROVED; symmetric=PROVED |
| `rat_lt` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | irreflexive=EXHAUSTIVE; asymmetric=HELD | irreflexive=PROVED; asymmetric=PROVED |
| `rat_floor` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | lower_bound=EXHAUSTIVE; upper_bound=EXHAUSTIVE | lower_bound=PROVED; upper_bound=PROVED |
| `rat_add_zero_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | add_zero=EXHAUSTIVE | add_zero=PROVED |
| `rat_mul_one_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | mul_one=EXHAUSTIVE | mul_one=PROVED |
| `rat_eq_normalize_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | equality_is_normal_form_equality=HELD | equality_is_normal_form_equality=PROVED |
| `rat_add_assoc_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | associative=HELD | associative=PROVED |
